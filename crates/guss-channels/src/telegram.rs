use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use guss_core::domain::SourceChannel;
use guss_core::people::{Action, Actor};
use guss_core::store::{Store, TimeWindow};
use guss_llm::LlmProvider;
use guss_scheduler::ReportSink;
use guss_voice::AudioFormat;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::types::{InputFile, UpdateKind};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::orchestrator::{Notify, Orchestrator, Outcome};
use crate::telegram_ux::{self, Command, TelegramAccess};
use crate::voice_support::VoiceSupport;

/// Delivers scheduled (EOD/weekly/monthly) reports to a fixed Telegram
/// chat - the business owner's own chat with the bot, configured once
/// via GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID.
pub struct TelegramReportSink {
    bot: Bot,
    chat_id: ChatId,
}

impl TelegramReportSink {
    pub fn new(bot_token: &str, chat_id: i64) -> Self {
        Self {
            bot: new_bot(bot_token),
            chat_id: ChatId(chat_id),
        }
    }
}

#[async_trait]
impl ReportSink for TelegramReportSink {
    async fn send_report(&self, text: String) -> Result<(), String> {
        send_text(&self.bot, self.chat_id, &text)
            .await
            .map_err(|e| e.to_string())
    }
}

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
const LONG_POLL_SECS: u32 = 30;

// teloxide's default client gives up after 17 s, shorter than the 30 s
// long poll: with a valid token every quiet poll timed out and Guss
// exited and restarted every ~18 s. The client must outlast the poll.
fn new_bot(token: &str) -> Bot {
    let client = teloxide::net::default_reqwest_settings()
        .timeout(Duration::from_secs(u64::from(LONG_POLL_SECS) + 15))
        .build()
        .expect("reqwest client builds with static settings");
    Bot::with_client(token, client)
}

fn is_transient(e: &RequestError) -> bool {
    matches!(
        e,
        RequestError::Network(_) | RequestError::Io(_) | RequestError::RetryAfter(_)
    )
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(failures.min(5)).min(30))
}

pub async fn run<P, S>(
    orchestrator: Arc<Orchestrator<P, S>>,
    voice: Arc<VoiceSupport>,
    bot_token: &str,
    access: TelegramAccess,
) -> Result<(), TelegramError>
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let bot = new_bot(bot_token);
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_MESSAGES));
    let mut tasks = JoinSet::new();
    let mut offset: i32 = 0;

    tracing::info!("Telegram channel starting (long polling)");
    if access.is_open() {
        tracing::warn!(
            "No Telegram owner or allowed users set: anyone who finds this bot can record entries.              Set GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID and/or GUSS_CHANNEL__TELEGRAM_ALLOWED_USER_IDS."
        );
    }
    {
        let business = orchestrator.business();
        telegram_ux::apply_profile(&bot, &business.name, &business.language).await;
    }
    let access = Arc::new(access);

    let mut failures: u32 = 0;
    loop {
        let updates = match bot
            .get_updates()
            .offset(offset)
            .timeout(LONG_POLL_SECS)
            .await
        {
            Ok(updates) => {
                failures = 0;
                updates
            }
            Err(e) if is_transient(&e) => {
                failures += 1;
                let wait = backoff(failures);
                tracing::warn!(error = %e, retry_in_secs = wait.as_secs(), "Telegram unreachable, retrying");
                tokio::time::sleep(wait).await;
                continue;
            }
            // A wrong token and other API refusals stay fatal, so a bad
            // setting shows up at startup instead of being retried forever.
            Err(e) => return Err(TelegramError::Api(e.to_string())),
        };

        for update in updates {
            offset = offset.max(update.id.0 as i32 + 1);

            if let UpdateKind::Message(msg) = update.kind {
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semaphore never closed");
                let orchestrator = orchestrator.clone();
                let voice = voice.clone();
                let bot = bot.clone();
                let access = access.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_message(&orchestrator, &voice, &bot, &access, msg).await
                    {
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
    access: &TelegramAccess,
    msg: teloxide::types::Message,
) -> Result<(), TelegramError>
where
    P: LlmProvider,
    S: Store,
{
    let chat_id = msg.chat.id;
    let conversation_key = chat_id.0.to_string();
    let provider_message_id = msg.id.0.to_string();
    let language = orchestrator.business().language.clone();
    let business_name = orchestrator.business().name.clone();

    // Groups would let anyone in them write to the books.
    if !msg.chat.is_private() {
        send_text(bot, chat_id, &telegram_ux::private_chats_only(&language)).await?;
        return Ok(());
    }
    let Some(user) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = user.id.0 as i64;
    let command = msg.text().and_then(telegram_ux::parse_command);

    if command == Some(Command::MyId) {
        send_text(bot, chat_id, &telegram_ux::your_id(&language, user_id)).await?;
        return Ok(());
    }
    let Some(role) = access.role_of(user_id) else {
        tracing::info!(user_id, "refused a message from a user who isn't allowed");
        send_text(
            bot,
            chat_id,
            &telegram_ux::private_bot(&language, &business_name, user_id),
        )
        .await?;
        return Ok(());
    };
    let name = match &user.last_name {
        Some(last) => format!("{} {last}", user.first_name),
        None => user.first_name.clone(),
    };
    let actor = Actor::new(format!("telegram:{user_id}"), name, role);
    let api = |e: crate::OrchestratorError| TelegramError::Api(e.to_string());

    match command {
        Some(Command::Start) => {
            let reply = telegram_ux::welcome(&language, &business_name, &user.first_name);
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Help) => {
            send_text(bot, chat_id, &telegram_ux::help(&language)).await?;
            return Ok(());
        }
        Some(Command::Report(arg)) => {
            let reply = match telegram_ux::report_window(arg.as_deref()) {
                Some(window) if window != TimeWindow::Day && !role.can(Action::ReportLonger) => {
                    crate::journal_texts::longer_reports_not_allowed(&language).to_string()
                }
                Some(window) => orchestrator.handle_report(window).await.map_err(api)?,
                None => telegram_ux::bad_report_window(&language),
            };
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::History) => {
            let reply = orchestrator.history(15).await.map_err(api)?;
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Pending) => {
            let reply = orchestrator.pending_voids(&actor).await.map_err(api)?;
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Void(arg)) => {
            let Some((number, reason)) = telegram_ux::number_and_rest(arg.as_deref()) else {
                send_text(bot, chat_id, &telegram_ux::void_usage(&language)).await?;
                return Ok(());
            };
            let outcome = orchestrator
                .void_entry(&actor, number, &reason)
                .await
                .map_err(api)?;
            deliver(bot, access, chat_id, outcome).await?;
            return Ok(());
        }
        Some(Command::Approve(ref arg)) | Some(Command::Reject(ref arg)) => {
            let approve = matches!(command, Some(Command::Approve(_)));
            let Some((number, _)) = telegram_ux::number_and_rest(arg.as_deref()) else {
                send_text(bot, chat_id, &telegram_ux::decide_usage(&language)).await?;
                return Ok(());
            };
            let outcome = orchestrator
                .decide_void(&actor, number, approve)
                .await
                .map_err(api)?;
            deliver(bot, access, chat_id, outcome).await?;
            return Ok(());
        }
        Some(Command::Unknown) => {
            send_text(bot, chat_id, &telegram_ux::unknown_command(&language)).await?;
            return Ok(());
        }
        Some(Command::Undo) | Some(Command::MyId) | None => {}
    }
    let undo_text = (command == Some(Command::Undo)).then(|| "undo".to_string());

    let text = if let Some(undo) = undo_text {
        undo
    } else if let Some(text) = msg.text() {
        text.to_string()
    } else if let Some(telegram_voice) = msg.voice() {
        match transcribe_voice_note(voice, bot, telegram_voice.file.id.clone()).await {
            Ok(transcript) => transcript,
            Err(e) => {
                send_text(
                    bot,
                    chat_id,
                    &format!("Couldn't understand that voice note: {e}"),
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        send_text(
            bot,
            chat_id,
            "I can only read text or voice notes right now.",
        )
        .await?;
        return Ok(());
    };

    let reply = orchestrator
        .handle_inbound_as(
            &actor,
            &conversation_key,
            SourceChannel::Telegram,
            &provider_message_id,
            &text,
        )
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
                if let Err(e) = bot
                    .send_audio(chat_id, InputFile::memory(clip.bytes).file_name(file_name))
                    .await
                {
                    tracing::warn!(error = %e, "failed to send TTS audio to Telegram");
                }
            }
            Err(e) => tracing::warn!(error = %e, "TTS synthesis failed for Telegram reply"),
        }
    }

    Ok(())
}

async fn transcribe_voice_note(
    voice: &VoiceSupport,
    bot: &Bot,
    file_id: teloxide::types::FileId,
) -> Result<String, TelegramError> {
    let Some(stt) = &voice.stt else {
        return Err(TelegramError::Api(
            "no speech-to-text provider is configured".to_string(),
        ));
    };

    let file = bot
        .get_file(file_id)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;
    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot.token(),
        file.path
    );
    let bytes = reqwest::get(&url)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;

    let wav = guss_voice::convert::to_wav16k_mono(&bytes)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;
    stt.transcribe(&wav)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))
}

/// Sends the reply, then the notices for other people: void requests to
/// the owner and managers (not back to the person asking), decisions to
/// the person who asked. A notice that can't be delivered is logged, never
/// fatal: the request is stored and visible in /pending either way.
async fn deliver(
    bot: &Bot,
    access: &TelegramAccess,
    chat_id: ChatId,
    outcome: Outcome,
) -> Result<(), TelegramError> {
    send_text(bot, chat_id, &outcome.reply).await?;
    for notice in outcome.notify {
        let (targets, text) = match notice {
            Notify::Approvers(text) => (access.approver_chats(), text),
            Notify::Person(id, text) => {
                match id.strip_prefix("telegram:").and_then(|n| n.parse().ok()) {
                    Some(n) => (vec![n], text),
                    None => continue,
                }
            }
        };
        for target in targets.into_iter().filter(|t| *t != chat_id.0) {
            if let Err(e) = send_text(bot, ChatId(target), &text).await {
                tracing::warn!(target, error = %e, "couldn't deliver a notice");
            }
        }
    }
    Ok(())
}

async fn send_text(bot: &Bot, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
    bot.send_message(chat_id, text)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;
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
    use serde_json::{Value, json};
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

        let message: Message =
            serde_json::from_value(sample_message_json(7, 42, "slow day today")).unwrap();

        let access = TelegramAccess::default();
        handle_message(&orchestrator, &voice, &bot, &access, message.clone())
            .await
            .unwrap();
        handle_message(&orchestrator, &voice, &bot, &access, message)
            .await
            .unwrap();

        server.verify().await;
    }

    async fn sent_texts(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn stranger_is_refused_and_nothing_is_recorded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": sample_message_json(1, 42, "reply")
            })))
            .mount(&server)
            .await;
        let bot = Bot::new("TEST_TOKEN").set_api_url(server.uri().parse().unwrap());
        let orchestrator = test_orchestrator().await;
        let access = TelegramAccess {
            manager_user_ids: vec![],
            owner_chat_id: Some(10),
            allowed_user_ids: vec![],
        };
        let message: Message =
            serde_json::from_value(sample_message_json(7, 42, "sold 3 rice at 120")).unwrap();

        handle_message(
            &orchestrator,
            &VoiceSupport::default(),
            &bot,
            &access,
            message,
        )
        .await
        .unwrap();

        let texts = sent_texts(&server).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("private") && texts[0].contains("42"),
            "{}",
            texts[0]
        );
    }

    #[tokio::test]
    async fn start_welcomes_an_allowed_user_without_recording() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": sample_message_json(1, 42, "reply")
            })))
            .mount(&server)
            .await;
        let bot = Bot::new("TEST_TOKEN").set_api_url(server.uri().parse().unwrap());
        let orchestrator = test_orchestrator().await;
        let access = TelegramAccess {
            manager_user_ids: vec![],
            owner_chat_id: Some(42),
            allowed_user_ids: vec![],
        };
        let message: Message =
            serde_json::from_value(sample_message_json(8, 42, "/start")).unwrap();

        handle_message(
            &orchestrator,
            &VoiceSupport::default(),
            &bot,
            &access,
            message,
        )
        .await
        .unwrap();

        let texts = sent_texts(&server).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].starts_with("Hi Test!") && texts[0].contains("/report"),
            "{}",
            texts[0]
        );
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(9), Duration::from_secs(30));
    }
}
