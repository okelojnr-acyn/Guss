//! Step-up checks (plan 16.4): before a sensitive action, the person
//! confirms it's them with their own passkey on a one-time Friday link,
//! then sends the same command again.
// shortcut: the open checks live in memory; a restart just asks for a new link.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use guss_core::money::CurrencyAmount;

use crate::friday::{FridayClient, FridayError};

/// Voids and approvals of entries worth more than this (in the business
/// currency) need a step-up; Friday's team-leader approval limit.
pub const ABOVE_MAJOR: f64 = 2_000.0;
const OPEN_FOR: Duration = Duration::from_secs(5 * 60);

/// An open check: what it's for, Friday's check id, the link, when asked.
type Open = (String, String, String, Instant);

/// actor id -> their open check.
static OPEN: LazyLock<Mutex<HashMap<String, Open>>> = LazyLock::new(Mutex::default);

/// Whether this much money needs a step-up (another currency always does).
pub fn needed(worth: Option<&CurrencyAmount>, home: &str) -> bool {
    worth.is_some_and(|w| {
        !w.currency.trim().eq_ignore_ascii_case(home) || w.major_units().abs() > ABOVE_MAJOR
    })
}

/// None: confirmed, go ahead. Some(reply): stop and say this (the link, or
/// why not).
pub async fn gate(
    friday: &FridayClient,
    actor_id: &str,
    telegram_id: i64,
    key: &str,
    what: &str,
) -> Option<String> {
    let open = OPEN.lock().ok()?.get(actor_id).cloned();
    if let Some((k, id, link, at)) = open
        && k == key
        && at.elapsed() < OPEN_FOR
    {
        return match friday.step_up_done(&id).await {
            Ok(true) => {
                OPEN.lock().ok()?.remove(actor_id);
                None
            }
            Ok(false) => Some(format!(
                "Not confirmed yet. Open this link and confirm with your passkey, then send the same command again:\n{link}"
            )),
            Err(e) => {
                tracing::warn!(error = %e, "couldn't check a step-up");
                Some("Friday didn't answer; try again in a moment.".into())
            }
        };
    }
    match friday.start_step_up(telegram_id, what).await {
        Ok((id, link)) => {
            if let Ok(mut m) = OPEN.lock() {
                m.insert(
                    actor_id.to_string(),
                    (key.to_string(), id, link.clone(), Instant::now()),
                );
            }
            Some(format!(
                "{what}: this needs you to confirm it's you. Open this link within 5 minutes and confirm with your passkey, then send the same command again:\n{link}"
            ))
        }
        Err(FridayError::Status { status: 409, body }) => Some(format!("{body}. Then try again.")),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't start a step-up");
            Some("Couldn't start the check with Friday; try again in a moment.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_or_foreign_amounts_need_a_step_up() {
        assert!(!needed(None, "KES"));
        assert!(!needed(Some(&CurrencyAmount::new(2000.0, "KES")), "KES"));
        assert!(needed(Some(&CurrencyAmount::new(2000.5, "KES")), "KES"));
        assert!(needed(Some(&CurrencyAmount::new(-30000.0, "KES")), "KES"));
        assert!(needed(Some(&CurrencyAmount::new(10.0, "USD")), "KES"));
    }

    #[tokio::test]
    async fn ask_once_then_go_ahead_after_the_passkey() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/guss/svc/step-up"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"id": "su-1", "link": "https://friday.example/step-up?c=abc"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let friday = FridayClient::new(server.uri(), "fgs_key");
        let first = gate(&friday, "telegram:9", 9, "void:12", "Void #12")
            .await
            .unwrap();
        assert!(
            first.contains("https://friday.example/step-up?c=abc"),
            "{first}"
        );

        let not_yet = Mock::given(method("GET"))
            .and(path("/guss/svc/step-up/su-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": false})))
            .up_to_n_times(1)
            .mount_as_scoped(&server)
            .await;
        let again = gate(&friday, "telegram:9", 9, "void:12", "Void #12")
            .await
            .unwrap();
        assert!(again.starts_with("Not confirmed yet"), "{again}");
        drop(not_yet);

        Mock::given(method("GET"))
            .and(path("/guss/svc/step-up/su-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": true})))
            .mount(&server)
            .await;
        assert_eq!(
            gate(&friday, "telegram:9", 9, "void:12", "Void #12").await,
            None
        );
    }
}
