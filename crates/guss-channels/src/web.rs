//! A small authenticated HTTP API so another app can talk to Guss - the
//! Friday widget calls it through Friday's backend (the token never
//! reaches a browser). Runs next to the main channel (Telegram etc.), not
//! instead of it, and uses the same orchestrator, so a sale typed in the
//! widget lands in the same ledger as one sent on Telegram.
//!
//! Routes:
//!   GET  /health            - no auth, for uptime checks
//!   POST /api/chat          - {conversation_id, text} -> {reply}
//!   GET  /api/report?window=day|week|month -> {reply}

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use guss_core::domain::SourceChannel;
use guss_core::store::{Store, TimeWindow};
use guss_llm::LlmProvider;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::orchestrator::Orchestrator;

const MAX_CONCURRENT: usize = 4;
const MAX_TEXT: usize = 2000;
const MAX_CONVERSATION_ID: usize = 128;

pub struct WebConfig {
    /// Shared secret; requests must send `Authorization: Bearer <token>`.
    pub token: String,
    pub listen_addr: String,
}

struct AppState<P: LlmProvider, S: Store> {
    orchestrator: Arc<Orchestrator<P, S>>,
    token: String,
    semaphore: Arc<Semaphore>,
}

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn router<P, S>(orchestrator: Arc<Orchestrator<P, S>>, token: String) -> Router
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let state = Arc::new(AppState {
        orchestrator,
        token,
        semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
    });
    Router::new()
        .route("/health", get(health::<P, S>))
        .route("/api/chat", post(chat::<P, S>))
        .route("/api/report", get(report::<P, S>))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(state)
}

pub async fn run<P, S>(
    orchestrator: Arc<Orchestrator<P, S>>,
    config: WebConfig,
) -> Result<(), WebError>
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let app = router(orchestrator, config.token);
    tracing::info!(listen_addr = %config.listen_addr, "web API starting");
    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Constant-time comparison so the token can't be guessed byte by byte
/// from response timing.
fn token_ok(expected: &str, headers: &HeaderMap) -> bool {
    let Some(given) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Serialize)]
struct Reply {
    reply: String,
}

fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    business: String,
}

async fn health<P: LlmProvider, S: Store>(
    State(state): State<Arc<AppState<P, S>>>,
) -> Json<Health> {
    Json(Health {
        status: "ok",
        business: state.orchestrator.business().name.clone(),
    })
}

#[derive(Deserialize)]
struct ChatRequest {
    conversation_id: String,
    text: String,
}

async fn chat<P, S>(
    State(state): State<Arc<AppState<P, S>>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    if !token_ok(&state.token, &headers) {
        return error(StatusCode::UNAUTHORIZED, "missing or wrong token");
    }
    let text = req.text.trim();
    if text.is_empty() || text.chars().count() > MAX_TEXT {
        return error(StatusCode::BAD_REQUEST, "text must be 1 to 2000 characters");
    }
    let id = req.conversation_id.trim();
    if id.is_empty()
        || id.len() > MAX_CONVERSATION_ID
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        return error(
            StatusCode::BAD_REQUEST,
            "conversation_id: up to 128 letters, digits, - _ :",
        );
    }
    let Ok(_permit) = state.semaphore.clone().try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Guss is busy, try again in a moment",
        );
    };
    match state
        .orchestrator
        .handle_message(&format!("web:{id}"), SourceChannel::Web, text)
        .await
    {
        Ok(reply) => Json(Reply { reply }).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "web chat failed");
            let message = e.user_message(&state.orchestrator.business().language);
            error(StatusCode::BAD_GATEWAY, &message)
        }
    }
}

#[derive(Deserialize)]
struct ReportQuery {
    window: Option<String>,
}

async fn report<P, S>(
    State(state): State<Arc<AppState<P, S>>>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Response
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    if !token_ok(&state.token, &headers) {
        return error(StatusCode::UNAUTHORIZED, "missing or wrong token");
    }
    let window = match q.window.as_deref().unwrap_or("day") {
        "day" => TimeWindow::Day,
        "week" => TimeWindow::Week,
        "month" => TimeWindow::Month,
        _ => return error(StatusCode::BAD_REQUEST, "window must be day, week or month"),
    };
    match state.orchestrator.handle_report(window).await {
        Ok(reply) => Json(Reply { reply }).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "web report failed");
            error(
                StatusCode::BAD_GATEWAY,
                "Guss couldn't build that report right now",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", v.parse().unwrap());
        h
    }

    #[test]
    fn token_check() {
        assert!(token_ok("s3cret", &headers("Bearer s3cret")));
        assert!(!token_ok("s3cret", &headers("Bearer s3creT")));
        assert!(!token_ok("s3cret", &headers("Bearer s3cre")));
        assert!(!token_ok("s3cret", &headers("s3cret")));
        assert!(!token_ok("", &headers("Bearer ")));
        assert!(!token_ok("s3cret", &HeaderMap::new()));
    }
}
