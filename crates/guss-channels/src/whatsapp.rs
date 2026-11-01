use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{body::Bytes, Router};
use guss_core::domain::SourceChannel;
use guss_core::store::Store;
use guss_llm::LlmProvider;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::orchestrator::Orchestrator;
use crate::voice_support::VoiceSupport;

#[derive(Debug, Error)]
pub enum WhatsAppError {
    #[error("WhatsApp Graph API error: {0}")]
    Api(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

const MAX_CONCURRENT_MESSAGES: usize = 8;
const GRAPH_API_VERSION: &str = "v21.0";

pub struct WhatsAppConfig {
    pub verify_token: String,
    pub access_token: String,
    pub phone_number_id: String,
    pub app_secret: String,
    pub listen_addr: String,
}

struct AppState<P: LlmProvider, S: Store> {
    orchestrator: Arc<Orchestrator<P, S>>,
    voice: Arc<VoiceSupport>,
    config: WhatsAppConfig,
    semaphore: Arc<Semaphore>,
}

/// Runs the WhatsApp Cloud API channel: an axum webhook server handling
/// Meta's verification handshake (GET) and inbound message events (POST),
/// each POST's signature checked against `X-Hub-Signature-256` before the
/// body is trusted - this is the compliant, non-ban-risk path the user
/// chose over an unofficial WhatsApp Web bridge.
pub async fn run<P, S>(
    orchestrator: Arc<Orchestrator<P, S>>,
    voice: Arc<VoiceSupport>,
    config: WhatsAppConfig,
) -> Result<(), WhatsAppError>
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let listen_addr = config.listen_addr.clone();
    let state = Arc::new(AppState { orchestrator, voice, config, semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_MESSAGES)) });

    let app = Router::new()
        .route("/webhook", get(verify_webhook::<P, S>))
        .route("/webhook", post(handle_webhook::<P, S>))
        .with_state(state);

    tracing::info!(listen_addr, "WhatsApp Cloud API channel starting");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct VerifyQuery {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
}

async fn verify_webhook<P: LlmProvider, S: Store>(State(state): State<Arc<AppState<P, S>>>, Query(q): Query<VerifyQuery>) -> impl IntoResponse {
    match (q.mode.as_deref(), &q.verify_token, q.challenge) {
        (Some("subscribe"), Some(token), Some(challenge)) if *token == state.config.verify_token => {
            (StatusCode::OK, challenge).into_response()
        }
        _ => (StatusCode::FORBIDDEN, "verify token mismatch").into_response(),
    }
}

fn signature_valid(app_secret: &str, signature_header: Option<&str>, raw_body: &[u8]) -> bool {
    let Some(header) = signature_header else { return false };
    let Some(hex_sig) = header.strip_prefix("sha256=") else { return false };
    let Ok(expected) = hex::decode(hex_sig) else { return false };

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) else { return false };
    mac.update(raw_body);
    mac.verify_slice(&expected).is_ok()
}

async fn handle_webhook<P, S>(State(state): State<Arc<AppState<P, S>>>, headers: HeaderMap, raw_body: Bytes) -> impl IntoResponse
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let signature = headers.get("X-Hub-Signature-256").and_then(|v| v.to_str().ok());
    if !signature_valid(&state.config.app_secret, signature, &raw_body) {
        tracing::warn!("rejected WhatsApp webhook POST with invalid/missing signature");
        return StatusCode::UNAUTHORIZED;
    }

    let payload: WebhookPayload = match serde_json::from_slice(&raw_body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse WhatsApp webhook payload");
            return StatusCode::OK; // ack anyway - Meta retries on non-2xx
        }
    };

    let mut tasks = JoinSet::new();
    for message in payload.into_messages() {
        let permit = state.semaphore.clone().acquire_owned().await.expect("semaphore never closed");
        let state = state.clone();
        tasks.spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_message(&state, message).await {
                tracing::warn!(error = %e, "failed to handle WhatsApp message");
            }
        });
    }
    while tasks.join_next().await.is_some() {}

    StatusCode::OK
}

#[derive(Debug, Deserialize)]
struct WebhookPayload {
    entry: Vec<WebhookEntry>,
}

#[derive(Debug, Deserialize)]
struct WebhookEntry {
    changes: Vec<WebhookChange>,
}

#[derive(Debug, Deserialize)]
struct WebhookChange {
    value: WebhookValue,
}

#[derive(Debug, Deserialize, Default)]
struct WebhookValue {
    #[serde(default)]
    messages: Vec<InboundMessage>,
}

#[derive(Debug, Deserialize)]
struct InboundMessage {
    from: String,
    id: String,
    #[serde(rename = "type")]
    kind: String,
    text: Option<InboundText>,
    audio: Option<InboundMedia>,
}

#[derive(Debug, Deserialize)]
struct InboundText {
    body: String,
}

#[derive(Debug, Deserialize)]
struct InboundMedia {
    id: String,
}

impl WebhookPayload {
    fn into_messages(self) -> Vec<InboundMessage> {
        self.entry
            .into_iter()
            .flat_map(|e| e.changes)
            .flat_map(|c| c.value.messages)
            .collect()
    }
}

async fn handle_message<P, S>(state: &AppState<P, S>, msg: InboundMessage) -> Result<(), WhatsAppError>
where
    P: LlmProvider,
    S: Store,
{
    let text = match msg.kind.as_str() {
        "text" => msg.text.map(|t| t.body).unwrap_or_default(),
        "audio" => {
            let Some(audio) = &msg.audio else {
                return send_text(state, &msg.from, "I couldn't read that voice note.").await;
            };
            match transcribe_media(state, &audio.id).await {
                Ok(transcript) => transcript,
                Err(e) => return send_text(state, &msg.from, &format!("Couldn't understand that voice note: {e}")).await,
            }
        }
        other => return send_text(state, &msg.from, &format!("I can only read text or voice notes right now (got \"{other}\").")).await,
    };

    let reply = state
        .orchestrator
        .handle_inbound(&msg.from, SourceChannel::WhatsApp, &msg.id, &text)
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?;

    let Some(reply_text) = reply else {
        return Ok(()); // redelivered webhook, already handled
    };

    send_text(state, &msg.from, &reply_text).await?;

    if let Some(tts) = &state.voice.tts {
        match tts.synthesize(&reply_text).await {
            Ok(clip) => {
                if let Err(e) = send_audio(state, &msg.from, clip).await {
                    tracing::warn!(error = %e, "failed to send TTS audio reply over WhatsApp");
                }
            }
            Err(e) => tracing::warn!(error = %e, "TTS synthesis failed for WhatsApp reply"),
        }
    }

    Ok(())
}

async fn transcribe_media<P: LlmProvider, S: Store>(state: &AppState<P, S>, media_id: &str) -> Result<String, WhatsAppError> {
    let Some(stt) = &state.voice.stt else {
        return Err(WhatsAppError::Api("no speech-to-text provider is configured".to_string()));
    };

    let client = reqwest::Client::new();
    let media_url = format!("https://graph.facebook.com/{GRAPH_API_VERSION}/{media_id}");
    let media_info: serde_json::Value = client
        .get(&media_url)
        .bearer_auth(&state.config.access_token)
        .send()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?
        .json()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?;

    let download_url = media_info
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| WhatsAppError::Api("media lookup response had no url".to_string()))?;

    let bytes = client
        .get(download_url)
        .bearer_auth(&state.config.access_token)
        .send()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?;

    let wav = guss_voice::convert::to_wav16k_mono(&bytes).await.map_err(|e| WhatsAppError::Api(e.to_string()))?;
    stt.transcribe(&wav).await.map_err(|e| WhatsAppError::Api(e.to_string()))
}

async fn send_text<P: LlmProvider, S: Store>(state: &AppState<P, S>, to: &str, body: &str) -> Result<(), WhatsAppError> {
    send_text_raw(&state.config.access_token, &state.config.phone_number_id, to, body).await
}

/// The actual Graph API call, factored out of `send_text` so the
/// scheduler's `WhatsAppReportSink` (which has no `AppState`, just raw
/// credentials) can reuse it for proactive EOD/weekly/monthly pushes.
async fn send_text_raw(access_token: &str, phone_number_id: &str, to: &str, body: &str) -> Result<(), WhatsAppError> {
    let client = reqwest::Client::new();
    let url = format!("https://graph.facebook.com/{GRAPH_API_VERSION}/{phone_number_id}/messages");
    let response = client
        .post(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "text",
            "text": {"body": body},
        }))
        .send()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(WhatsAppError::Api(format!("send message HTTP {status}: {text}")));
    }
    Ok(())
}

/// Delivers scheduled (EOD/weekly/monthly) reports to a fixed WhatsApp
/// number - the business owner's own number, configured once via
/// GUSS_CHANNEL__WHATSAPP_OWNER_PHONE_NUMBER.
pub struct WhatsAppReportSink {
    access_token: String,
    phone_number_id: String,
    to: String,
}

impl WhatsAppReportSink {
    pub fn new(access_token: impl Into<String>, phone_number_id: impl Into<String>, to: impl Into<String>) -> Self {
        Self { access_token: access_token.into(), phone_number_id: phone_number_id.into(), to: to.into() }
    }
}

#[async_trait::async_trait]
impl guss_scheduler::ReportSink for WhatsAppReportSink {
    async fn send_report(&self, text: String) -> Result<(), String> {
        send_text_raw(&self.access_token, &self.phone_number_id, &self.to, &text).await.map_err(|e| e.to_string())
    }
}

async fn send_audio<P: LlmProvider, S: Store>(state: &AppState<P, S>, to: &str, clip: guss_voice::AudioClip) -> Result<(), WhatsAppError> {
    let client = reqwest::Client::new();
    let mime = match clip.format {
        guss_voice::AudioFormat::Wav => "audio/wav",
        guss_voice::AudioFormat::Mp3 => "audio/mpeg",
    };

    let upload_url = format!(
        "https://graph.facebook.com/{GRAPH_API_VERSION}/{}/media",
        state.config.phone_number_id
    );
    let part = reqwest::multipart::Part::bytes(clip.bytes).mime_str(mime).map_err(|e| WhatsAppError::Api(e.to_string()))?;
    let form = reqwest::multipart::Form::new()
        .text("messaging_product", "whatsapp")
        .text("type", mime)
        .part("file", part);

    let upload: serde_json::Value = client
        .post(&upload_url)
        .bearer_auth(&state.config.access_token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?
        .json()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?;

    let media_id = upload
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WhatsAppError::Api(format!("media upload response had no id: {upload}")))?;

    let send_url = format!(
        "https://graph.facebook.com/{GRAPH_API_VERSION}/{}/messages",
        state.config.phone_number_id
    );
    let response = client
        .post(&send_url)
        .bearer_auth(&state.config.access_token)
        .json(&serde_json::json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "audio",
            "audio": {"id": media_id},
        }))
        .send()
        .await
        .map_err(|e| WhatsAppError::Api(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(WhatsAppError::Api(format!("send audio HTTP {status}: {text}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_check_accepts_correctly_signed_body() {
        let secret = "test-secret";
        let body = b"{\"hello\":\"world\"}";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());
        let header = format!("sha256={sig}");

        assert!(signature_valid(secret, Some(&header), body));
    }

    #[test]
    fn signature_check_rejects_wrong_secret() {
        let body = b"{\"hello\":\"world\"}";
        let mut mac = Hmac::<Sha256>::new_from_slice(b"right-secret").unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());
        let header = format!("sha256={sig}");

        assert!(!signature_valid("wrong-secret", Some(&header), body));
    }

    #[test]
    fn signature_check_rejects_missing_header() {
        assert!(!signature_valid("secret", None, b"body"));
    }

    #[test]
    fn signature_check_rejects_tampered_body() {
        let secret = "test-secret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"original body");
        let sig = hex::encode(mac.finalize().into_bytes());
        let header = format!("sha256={sig}");

        assert!(!signature_valid(secret, Some(&header), b"tampered body"));
    }

    #[test]
    fn webhook_payload_flattens_nested_messages() {
        let json = serde_json::json!({
            "entry": [{
                "id": "123",
                "changes": [{
                    "value": {
                        "messaging_product": "whatsapp",
                        "messages": [{
                            "from": "15550001234",
                            "id": "wamid.ABC",
                            "type": "text",
                            "text": {"body": "sold 3 bags of rice"}
                        }]
                    },
                    "field": "messages"
                }]
            }]
        });
        let payload: WebhookPayload = serde_json::from_value(json).unwrap();
        let messages = payload.into_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "15550001234");
        assert_eq!(messages[0].text.as_ref().unwrap().body, "sold 3 bags of rice");
    }
}
