use std::io::{BufRead, Write};

use guss_core::domain::SourceChannel;
use guss_core::store::{Store, TimeWindow};
use guss_llm::LlmProvider;
use guss_voice::{AudioFormat, Tts};

use crate::orchestrator::Orchestrator;
use crate::voice_support::VoiceSupport;

const CLI_CONVERSATION_KEY: &str = "cli";

async fn speak_reply(tts: &dyn Tts, reply: &str, output: &mut impl Write) {
    match tts.synthesize(reply).await {
        Ok(clip) => {
            let ext = match clip.format {
                AudioFormat::Wav => "wav",
                AudioFormat::Mp3 => "mp3",
            };
            let path = format!("guss_reply_{}.{ext}", chrono::Utc::now().timestamp_millis());
            if let Err(e) = tokio::fs::write(&path, &clip.bytes).await {
                let _ = writeln!(output, "(voice synthesis saved failed: {e})");
            } else {
                let _ = writeln!(output, "(voice reply saved to {path})");
            }
        }
        Err(e) => {
            let _ = writeln!(output, "(voice synthesis failed: {e})");
        }
    }
}

fn parse_report_window(rest: &str) -> TimeWindow {
    match rest.trim().to_lowercase().as_str() {
        "week" => TimeWindow::Week,
        "month" => TimeWindow::Month,
        _ => TimeWindow::Day,
    }
}

/// Runs the CLI channel's REPL loop over any Read/Write pair, so the same
/// code path is used for the real `stdin`/`stdout` and for a headless
/// integration test driving it against an in-memory buffer.
pub async fn run<P: LlmProvider, S: Store>(
    orchestrator: &Orchestrator<P, S>,
    voice: &VoiceSupport,
    mut input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    writeln!(
        output,
        "Guss is ready for \"{}\". Type what happened in the business, \"report [day|week|month]\", \"voice <path to audio file>\", or \"exit\".",
        orchestrator.business().name
    )?;

    let mut line = String::new();
    loop {
        write!(output, "> ")?;
        output.flush()?;
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if text.eq_ignore_ascii_case("exit") || text.eq_ignore_ascii_case("quit") {
            break;
        }

        let reply = if let Some(path) = text.strip_prefix("voice ") {
            match transcribe_file(voice, path.trim()).await {
                Ok(transcript) => {
                    writeln!(output, "(heard: {transcript})")?;
                    orchestrator.handle_message(CLI_CONVERSATION_KEY, SourceChannel::Cli, &transcript).await
                }
                Err(e) => Err(e.into()),
            }
        } else if let Some(rest) = text.to_lowercase().strip_prefix("report") {
            orchestrator.handle_report(parse_report_window(rest)).await
        } else {
            orchestrator.handle_message(CLI_CONVERSATION_KEY, SourceChannel::Cli, text).await
        };

        match reply {
            Ok(reply_text) => {
                writeln!(output, "{reply_text}")?;
                if let Some(tts) = &voice.tts {
                    speak_reply(tts.as_ref(), &reply_text, &mut output).await;
                }
            }
            Err(e) => writeln!(output, "Something went wrong: {e}")?,
        }
    }

    Ok(())
}

async fn transcribe_file(voice: &VoiceSupport, path: &str) -> Result<String, crate::orchestrator::OrchestratorError> {
    let Some(stt) = &voice.stt else {
        return Err(crate::orchestrator::OrchestratorError::VoiceNotConfigured(
            "no speech-to-text provider is configured (set GUSS_STT__* in .env)".to_string(),
        ));
    };
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| crate::orchestrator::OrchestratorError::VoiceNotConfigured(format!("couldn't read {path}: {e}")))?;
    let wav = guss_voice::convert::to_wav16k_mono(&bytes)
        .await
        .map_err(|e| crate::orchestrator::OrchestratorError::VoiceNotConfigured(e.to_string()))?;
    stt.transcribe(&wav)
        .await
        .map_err(|e| crate::orchestrator::OrchestratorError::VoiceNotConfigured(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use guss_core::domain::BusinessProfile;
    use guss_llm::{JsonCompletionRequest, LlmError};
    use guss_nlu::Nlu;
    use guss_store::SqliteStore;
    use serde_json::{json, Value};
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use uuid::Uuid;

    /// A canned LLM: first call extracts a sale, every call after that
    /// (e.g. for "report") returns an empty event list so the orchestrator
    /// doesn't misinterpret the report command as a new utterance if it
    /// ever reached the LLM (it shouldn't, since "report" is intercepted
    /// before extraction - this is just a safe default).
    struct CannedProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete_json(&self, _req: JsonCompletionRequest) -> Result<Value, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(json!({
                    "events": [
                        {"kind": "sale", "item_name": "rice", "quantity": 3.0, "unit_price_major": 500.0, "currency": null, "category": null}
                    ]
                }))
            } else {
                Ok(json!({"events": []}))
            }
        }
    }

    #[tokio::test]
    async fn sale_then_report_round_trips_through_cli() {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let business = BusinessProfile {
            id: Uuid::new_v4(),
            name: "Acme Shop".into(),
            default_currency: "USD".into(),
            language: "en".into(),
            timezone: "UTC".into(),
        };
        let nlu = Nlu::new(CannedProvider { calls: AtomicUsize::new(0) }, "en");
        let orchestrator = Orchestrator::new(nlu, store, business);

        let input = Cursor::new(b"sold 3 bags of rice at 500 each\nreport\nexit\n".to_vec());
        let mut output = Vec::new();
        run(&orchestrator, &VoiceSupport::default(), input, &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Sold: 3 rice"), "expected sale confirmation, got:\n{text}");
        assert!(text.contains("Reply \"no\" within 5 minutes to undo"));
        assert!(text.contains("Best sellers by quantity: rice"), "expected report to mention rice, got:\n{text}");
        assert!(text.contains("1500.00 USD"), "expected report revenue total, got:\n{text}");
    }

    #[tokio::test]
    async fn undo_within_window_removes_the_logged_sale() {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let business = BusinessProfile {
            id: Uuid::new_v4(),
            name: "Acme Shop".into(),
            default_currency: "USD".into(),
            language: "en".into(),
            timezone: "UTC".into(),
        };
        let nlu = Nlu::new(CannedProvider { calls: AtomicUsize::new(0) }, "en");
        let orchestrator = Orchestrator::new(nlu, store, business);

        let input = Cursor::new(b"sold 3 bags of rice at 500 each\nno\nreport\nexit\n".to_vec());
        let mut output = Vec::new();
        run(&orchestrator, &VoiceSupport::default(), input, &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Undone."));
        assert!(!text.contains("Best sellers by quantity"), "report should have nothing to show after undo, got:\n{text}");
    }
}
