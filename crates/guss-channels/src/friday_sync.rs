//! Sends Guss's entries to Friday Inventory: a sale lowers stock, a
//! restock raises it, a write-off lowers it, and a voided entry is
//! reversed. Each move is made *as the person who recorded the entry*
//! (their linked Friday account), tagged "guss:<entry id>" so a retry
//! never counts twice. Nothing is lost if Friday is down: entries wait in
//! the journal and go on the next round.

use std::time::Duration;

use guss_core::domain::BusinessEvent;
use guss_core::store::{Store, StoreError, SyncItem};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::friday::{FridayClient, FridayError, telegram_id_of};
use crate::items::normalize_name;

pub const SYNC_EVERY: Duration = Duration::from_secs(30);
const BATCH: i64 = 50;

/// Why an entry didn't reach Friday. `permanent` ones are shown in
/// /history and not retried.
#[derive(Debug, PartialEq)]
struct Failure {
    message: String,
    permanent: bool,
}

impl From<FridayError> for Failure {
    fn from(e: FridayError) -> Self {
        Failure {
            permanent: !e.is_transient(),
            message: e.to_string(),
        }
    }
}

fn permanent(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        permanent: true,
    }
}

/// One pass over the queue. Returns how many entries reached Friday.
pub async fn sync_once<S: Store + ?Sized>(
    store: &S,
    friday: &FridayClient,
    business_id: Uuid,
) -> Result<usize, StoreError> {
    let mut done = 0;
    for item in store.to_sync(business_id, BATCH).await? {
        let id = item.entry.event.id();
        match push(friday, &item).await {
            Ok(()) => {
                store.mark_synced(id, item.reverse).await?;
                done += 1;
            }
            Err(f) => {
                store.mark_sync_failed(id, &f.message, f.permanent).await?;
                if !f.permanent {
                    // Friday is down or slow: stop this round, retry later.
                    tracing::warn!(error = %f.message, "Friday sync paused");
                    break;
                }
            }
        }
    }
    Ok(done)
}

/// Runs `sync_once` forever, every `SYNC_EVERY`.
pub async fn run<S: Store + ?Sized>(store: &S, friday: &FridayClient, business_id: Uuid) {
    loop {
        match sync_once(store, friday, business_id).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(entries = n, "sent to Friday"),
            Err(e) => tracing::warn!(error = %e, "Friday sync couldn't read the journal"),
        }
        tokio::time::sleep(SYNC_EVERY).await;
    }
}

/// What a journal entry means for Inventory: (item, base-unit change, reason).
fn stock_change(event: &BusinessEvent) -> Option<(&str, f64, &'static str)> {
    match event {
        BusinessEvent::Sale(s) => Some((&s.item_name, -s.base_quantity(), "sale")),
        BusinessEvent::StockUpdate(su) => {
            let reason = if su.delta >= 0.0 {
                "received"
            } else {
                "write_off"
            };
            Some((&su.item_name, su.base_delta(), reason))
        }
        _ => None,
    }
}

async fn push(friday: &FridayClient, item: &SyncItem) -> Result<(), Failure> {
    let entry = &item.entry;
    let Some((name, change, reason)) = stock_change(&entry.event) else {
        return Ok(());
    };
    let people = friday.people().await?;
    if !people.can_write("inventory") {
        return Err(permanent(
            "Guss may not change Inventory (Friday: Integrations, Guss)",
        ));
    }
    let who = entry
        .recorded_by
        .clone()
        .unwrap_or_else(|| "someone".into());
    let Some(telegram_id) = entry.recorded_by_id.as_deref().and_then(telegram_id_of) else {
        return Err(permanent(format!(
            "recorded by {who}, who isn't linked to Friday"
        )));
    };
    let Some(token) = friday.token_for(telegram_id).await? else {
        return Err(permanent(format!(
            "{who} isn't linked to Friday (send /link)"
        )));
    };

    let delta = if item.reverse { -change } else { change };
    if (delta - delta.round()).abs() > 1e-9 {
        return Err(permanent(format!(
            "Friday Inventory counts whole units; {} {name} wasn't sent",
            delta.abs()
        )));
    }
    let id = entry.event.id();
    let reference = if item.reverse {
        format!("guss-void:{id}")
    } else {
        format!("guss:{id}")
    };

    // Already there (an earlier attempt got through but its answer
    // didn't): done.
    let existing = friday
        .get(
            &token,
            &format!("/api/inventory/stock_moves?reference={reference}"),
        )
        .await?;
    if existing.as_array().is_some_and(|a| !a.is_empty()) {
        return Ok(());
    }

    let product_id = find_or_create_product(friday, &token, name).await?;
    friday
        .post(
            &token,
            &format!("/api/inventory/products/{product_id}/adjust-stock"),
            &json!({
                "delta": delta.round() as i64,
                // A reversal is a correction, whatever the original was.
                "reason": if item.reverse { "manual_adjustment" } else { reason },
                "reference": reference,
            }),
        )
        .await?;
    Ok(())
}

/// The Inventory product with this name (any spelling Guss treats as the
/// same item), created if Friday doesn't have it yet.
async fn find_or_create_product(
    friday: &FridayClient,
    token: &str,
    name: &str,
) -> Result<String, Failure> {
    let wanted = normalize_name(name);
    let products = friday.get(token, "/api/inventory/products").await?;
    if let Some(found) = products.as_array().into_iter().flatten().find(|p| {
        p.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| normalize_name(n) == wanted)
    }) {
        return found
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| permanent("Friday sent a product without an id"));
    }
    let created = friday
        .post(
            token,
            "/api/inventory/products",
            &json!({"name": name, "kind": "product"}),
        )
        .await?;
    created
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| permanent("Friday created a product without an id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use guss_core::domain::{Sale, SourceChannel, StockUpdate};
    use guss_core::money::CurrencyAmount;
    use guss_core::people::{Actor, Role};
    use guss_store::SqliteStore;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "fgs_key";
    const TOKEN: &str = "jwt-for-mary";

    fn restock(business_id: Uuid, item: &str, delta: f64) -> BusinessEvent {
        BusinessEvent::StockUpdate(StockUpdate {
            id: Uuid::new_v4(),
            business_id,
            item_name: item.into(),
            delta,
            unit: Some("crate".into()),
            unit_factor: 24.0,
            unit_cost: None,
            reorder_threshold: None,
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Telegram,
            raw_text: None,
        })
    }

    fn sale(business_id: Uuid, item: &str, qty: f64) -> BusinessEvent {
        BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id,
            item_name: item.into(),
            quantity: qty,
            unit: None,
            unit_factor: 1.0,
            unit_price: CurrencyAmount::new(80.0, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Telegram,
            raw_text: None,
        })
    }

    async fn friday(access: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/guss/svc/people"))
            .and(header("authorization", format!("Bearer {KEY}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "people": [{"telegram_id": 7, "telegram_name": "Mary", "guss_role": "staff"}],
                "access": {"inventory": access}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/guss/svc/token"))
            .and(body_partial_json(json!({"telegram_id": 7})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"token": TOKEN, "expires_in": 600})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/guss/svc/token"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not linked"})))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn entries_reach_inventory_once_as_their_author_and_voids_reverse() {
        let server = friday("write").await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let crates = restock(business_id, "Sodas", 2.0);
        store.save_event_by(&crates, Some(&mary)).await.unwrap();

        // Friday has "soda" already; nothing with this reference yet.
        Mock::given(method("GET"))
            .and(path("/api/inventory/stock_moves"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/inventory/products"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"id": "p-soda", "name": "Soda"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/inventory/products/p-soda/adjust-stock"))
            .and(body_partial_json(json!({
                "delta": 48, "reason": "received", "reference": format!("guss:{}", crates.id())
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "p-soda"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(sync_once(&store, &client, business_id).await.unwrap(), 1);
        assert_eq!(sync_once(&store, &client, business_id).await.unwrap(), 0);

        // Voided after it reached Friday: the reversal goes out.
        let boss = Actor::new("telegram:1", "Boss", Role::Owner);
        store.void_event(crates.id(), &boss, "wrong").await.unwrap();
        Mock::given(method("POST"))
            .and(path("/api/inventory/products/p-soda/adjust-stock"))
            .and(body_partial_json(json!({
                "delta": -48, "reason": "manual_adjustment",
                "reference": format!("guss-void:{}", crates.id())
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "p-soda"})))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(sync_once(&store, &client, business_id).await.unwrap(), 1);
        assert!(store.to_sync(business_id, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn without_write_access_or_a_link_nothing_is_sent_and_history_says_why() {
        let server = friday("read").await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        store
            .save_event_by(&sale(business_id, "soda", 2.0), Some(&mary))
            .await
            .unwrap();
        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(sync_once(&store, &client, business_id).await.unwrap(), 0);
        let journal = store.journal(business_id, 5).await.unwrap();
        assert_eq!(
            journal[0].sync_error.as_deref(),
            Some("Guss may not change Inventory (Friday: Integrations, Guss)")
        );
        assert!(store.to_sync(business_id, 10).await.unwrap().is_empty());

        // Write access, but the author isn't linked.
        let server = friday("write").await;
        let client = FridayClient::new(server.uri(), KEY);
        let stranger = Actor::new("telegram:99", "Sam", Role::Staff);
        store
            .save_event_by(&sale(business_id, "soda", 1.0), Some(&stranger))
            .await
            .unwrap();
        sync_once(&store, &client, business_id).await.unwrap();
        let journal = store.journal(business_id, 5).await.unwrap();
        assert_eq!(
            journal[0].sync_error.as_deref(),
            Some("Sam isn't linked to Friday (send /link)")
        );
    }

    #[tokio::test]
    async fn friday_down_keeps_entries_queued() {
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        store
            .save_event_by(&sale(business_id, "soda", 2.0), Some(&mary))
            .await
            .unwrap();
        // Nothing listens here.
        let client = FridayClient::new("http://127.0.0.1:9", KEY);
        assert_eq!(sync_once(&store, &client, business_id).await.unwrap(), 0);
        let queue = store.to_sync(business_id, 10).await.unwrap();
        assert_eq!(queue.len(), 1, "kept for the next round");
        assert!(queue[0].entry.sync_error.is_some());
    }
}
