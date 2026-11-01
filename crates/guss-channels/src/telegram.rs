use std::sync::Arc;

use guss_core::domain::SourceChannel;
use guss_core::store::Store;
use guss_llm::LlmProvider;
use guss_voice::AudioFormat;
use teloxide::prelude::*;
use teloxide::types::{InputFile, UpdateKind};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::orchestrator::Orchestrator;
use crate::voice_support::VoiceSupport;

#[derive(Debug, Error)]
pub enum TelegramError {
    #[error("Telegram API error: {0}")]
    Api(String),
}

/// How many inbound messages can be processed concurrently. Bounds memory
/// and outbound LLM/TTS calls under a message burst, rather than spawning
/// one task per update unboundedly.
const MAX_CONCURRENT_MESSAGES: usize = 8;

/// Runs the Telegram channel via long polling. Each update is deduplicated
/// by its message id (`Orchestrator::handle_inbound`) before processing,
/// since Telegram can redeliver updates on timeout - and processing is
/// capped at `MAX_CONCURRENT_MESSAGES` concurrent tasks via a semaphore
/// acquired before each spawn, so a burst of messages can't spawn
/// unboundedly many tasks.
pub async fn run<P, S>(orchestrator: Arc<Orchestrator<P, S>>, voice: Arc<VoiceSupport>, bot_token: &str) -> Result<(), TelegramError>
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let bot = Bot::new(bot_token);
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_MESSAGES));
    let mut tasks = JoinSet::new();
    let mut offset: i32 = 0;

    tracing::info!("Telegram channel starting (long polling)");

    loop {
        let updates = bot
            .get_updates()
            .offset(offset)
            .timeout(30)
            .await
            .map_err(|e| TelegramError::Api(e.to_string()))?;

        for update in updates {
            offset = offset.max(update.id.0 as i32 + 1);

            if let UpdateKind::Message(msg) = update.kind {
                let permit = semaphore.clone().acquire_owned().await.expect("semaphore never closed");
                let orchestrator = orchestrator.clone();
                let voice = voice.clone();
                let bot = bot.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_message(&orchestrator, &voice, &bot, msg).await {
                        tracing::warn!(error = %e, "failed to handle Telegram message");
                    }
                });
            }
        }

        // Drain finished tasks so the JoinSet doesn't grow unboundedly
        // across polling iterations.
        while tasks.try_join_next().is_some() {}
    }
}

async fn handle_message<P, S>(
    orchestrator: &Orchestrator<P, S>,
    voice: &VoiceSupport,
    bot: &Bot,
    msg: teloxide::types::Message,
) -> Result<(), TelegramError>
where
    P: LlmProvider,
    S: Store,
{
    let chat_id = msg.chat.id;
    let conversation_key = chat_id.0.to_string();
    let provider_message_id = msg.id.0.to_string();

    let text = if let Some(text) = msg.text() {
        text.to_string()
    } else if let Some(telegram_voice) = msg.voice() {
        match transcribe_voice_note(voice, bot, telegram_voice.file.id.clone()).await {
            Ok(transcript) => transcript,
            Err(e) => {
                send_text(bot, chat_id, &format!("Couldn't understand that voice note: {e}")).await?;
                return Ok(());
            }
        }
    } else {
        send_text(bot, chat_id, "I can only read text or voice notes right now.").await?;
        return Ok(());
    };

    let reply = orchestrator
        .handle_inbound(&conversation_key, SourceChannel::Telegram, &provider_message_id, &text)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;

    let Some(reply_text) = reply else {
        return Ok(()); // redelivered update, already handled
    };

    send_text(bot, chat_id, &reply_text).await?;

    if let Some(tts) = &voice.tts {
        match tts.synthesize(&reply_text).await {
            Ok(clip) => {
                let file_name = match clip.format {
                    AudioFormat::Wav => "reply.wav",
                    AudioFormat::Mp3 => "reply.mp3",
                };
                // Sent via send_audio rather than send_voice: Telegram's
                // "voice message" bubble requires OGG/Opus specifically,
                // which Guss's TTS backends don't produce - this plays as
                // a normal audio attachment instead.
                if let Err(e) = bot.send_audio(chat_id, InputFile::memory(clip.bytes).file_name(file_name)).await {
                    tracing::warn!(error = %e, "failed to send TTS audio to Telegram");
                }
            }
            Err(e) => tracing::warn!(error = %e, "TTS synthesis failed for Telegram reply"),
        }
    }

    Ok(())
}

async fn transcribe_voice_note(voice: &VoiceSupport, bot: &Bot, file_id: teloxide::types::FileId) -> Result<String, TelegramError> {
    let Some(stt) = &voice.stt else {
        return Err(TelegramError::Api("no speech-to-text provider is configured".to_string()));
    };

    let file = bot.get_file(file_id).await.map_err(|e| TelegramError::Api(e.to_string()))?;
    let url = format!("https://api.telegram.org/file/bot{}/{}", bot.token(), file.path);
    let bytes = reqwest::get(&url)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;

    let wav = guss_voice::convert::to_wav16k_mono(&bytes).await.map_err(|e| TelegramError::Api(e.to_string()))?;
    stt.transcribe(&wav).await.map_err(|e| TelegramError::Api(e.to_string()))
}

async fn send_text(bot: &Bot, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
    bot.send_message(chat_id, text).await.map_err(|e| TelegramError::Api(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use guss_core::domain::BusinessProfile;
    use guss_llm::{JsonCompletionRequest, LlmError, LlmProvider};
    use guss_nlu::Nlu;
    use guss_store::SqliteStore;
    use serde_json::{json, Value};
    use teloxide::types::Message;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct EmptyProvider;

    #[async_trait]
    impl LlmProvider for EmptyProvider {
        async fn complete_json(&self, _req: JsonCompletionRequest) -> Result<Value, LlmError> {
            Ok(json!({"events": [{"kind": "note", "text": "noted"}]}))
        }
    }

    fn sample_message_json(message_id: i32, chat_id: i64, text: &str) -> Value {
        json!({
            "message_id": message_id,
            "date": 1700000000,
            "chat": {"id": chat_id, "type": "private", "first_name": "Test"},
            "from": {"id": chat_id, "is_bot": false, "first_name": "Test"},
            "text": text,
        })
    }

    async fn test_orchestrator() -> Orchestrator<EmptyProvider, SqliteStore> {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let business = BusinessProfile {
            id: uuid::Uuid::new_v4(),
            name: "Acme Shop".into(),
            default_currency: "USD".into(),
            language: "en".into(),
            timezone: "UTC".into(),
        };
        let nlu = Nlu::new(EmptyProvider, "en");
        Orchestrator::new(nlu, store, business)
    }

    #[tokio::test]
    async fn redelivered_message_is_not_replied_to_twice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": sample_message_json(1, 42, "reply")
            })))
            .expect(1) // exactly one send, even though handle_message runs twice on the same update
            .mount(&server)
            .await;

        let bot = Bot::new("TEST_TOKEN").set_api_url(server.uri().parse().unwrap());
        let orchestrator = test_orchestrator().await;
        let voice = VoiceSupport::default();

        let message: Message = serde_json::from_value(sample_message_json(7, 42, "slow day today")).unwrap();

        handle_message(&orchestrator, &voice, &bot, message.clone()).await.unwrap();
        handle_message(&orchestrator, &voice, &bot, message).await.unwrap();

        server.verify().await;
    }
}
