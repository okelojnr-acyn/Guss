//! Sends Guss's entries to Friday, as *the person who recorded each one*
//! (their linked Friday account), to every app the org lets Guss write:
//!
//! - Inventory: a sale lowers stock, a restock raises it, a write-off
//!   lowers it.
//! - Accounting: a cash sale (Dr Cash, Cr Sales Revenue), an expense (Dr
//!   Shop expenses, Cr Cash) and a paid restock (Dr Inventory, Cr Cash), as
//!   draft journal entries the accountant posts.
//!
//! A voided entry is reversed in both. Everything is tagged
//! "guss:<entry id>" and checked first, so a retry never counts twice.
//! Nothing is lost if Friday is down: entries wait in the journal and go
//! on the next round.

use std::time::Duration;

use chrono::{DateTime, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile};
use guss_core::store::{Store, StoreError, SyncItem};
use serde_json::{Value, json};

use crate::friday::{FridayClient, FridayError, telegram_id_of};
use crate::items::normalize_name;

pub const SYNC_EVERY: Duration = Duration::from_secs(30);
const BATCH: i64 = 50;

/// Accounting accounts Guss books into (Friday's starter chart, plus one
/// expense account Guss adds the first time it needs it).
const CASH: &str = "1010";
const INVENTORY_ASSET: &str = "1400";
const SALES_REVENUE: &str = "4000";
const SHOP_EXPENSES: (&str, &str) = ("6900", "Shop expenses");

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
    business: &BusinessProfile,
) -> Result<usize, StoreError> {
    let mut done = 0;
    for item in store.to_sync(business.id, BATCH).await? {
        let id = item.entry.event.id();
        match push(friday, business, &item).await {
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
pub async fn run<S: Store + ?Sized>(store: &S, friday: &FridayClient, business: BusinessProfile) {
    loop {
        match sync_once(store, friday, &business).await {
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

/// What a journal entry means for Accounting: (debit account, credit
/// account, amount in cents, currency, memo). None: nothing to book (a
/// write-off, a note, a restock without a cost).
fn booking(event: &BusinessEvent) -> Option<(&'static str, &'static str, i64, String, String)> {
    let cents = |major: f64| (major * 100.0).round() as i64;
    match event {
        BusinessEvent::Sale(s) => {
            let total = s.total();
            Some((
                CASH,
                SALES_REVENUE,
                cents(total.major_units()),
                total.currency,
                format!("Guss: sold {} {}", s.quantity, s.item_name),
            ))
        }
        BusinessEvent::Expense(x) => Some((
            SHOP_EXPENSES.0,
            CASH,
            cents(x.amount.major_units()),
            x.amount.currency.clone(),
            format!("Guss: {}", x.description),
        )),
        BusinessEvent::StockUpdate(su) if su.delta > 0.0 => su.unit_cost.as_ref().map(|c| {
            (
                INVENTORY_ASSET,
                CASH,
                cents(su.delta * c.major_units()),
                c.currency.clone(),
                format!("Guss: bought {} {}", su.delta, su.item_name),
            )
        }),
        _ => None,
    }
}

fn occurred_at(event: &BusinessEvent) -> DateTime<Utc> {
    match event {
        BusinessEvent::Sale(s) => s.occurred_at,
        BusinessEvent::Expense(x) => x.occurred_at,
        BusinessEvent::StockUpdate(su) => su.occurred_at,
        BusinessEvent::Note(n) => n.occurred_at,
    }
}

async fn push(
    friday: &FridayClient,
    business: &BusinessProfile,
    item: &SyncItem,
) -> Result<(), Failure> {
    let entry = &item.entry;
    let stock = stock_change(&entry.event);
    let book = booking(&entry.event);
    if stock.is_none() && book.is_none() {
        return Ok(());
    }
    let people = friday.people().await?;
    let to_inventory = stock.is_some() && people.can_write("inventory");
    let to_accounting = book.is_some() && people.can_write("accounting");
    if !to_inventory && !to_accounting {
        return Err(permanent(
            "Guss may not change Inventory or Accounting (Friday: Integrations, Guss)",
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
    let id = entry.event.id();
    let reference = if item.reverse {
        format!("guss-void:{id}")
    } else {
        format!("guss:{id}")
    };

    if let (true, Some((name, change, reason))) = (to_inventory, stock) {
        let delta = if item.reverse { -change } else { change };
        // Friday keeps stock to three decimals (grams, millilitres) since
        // units of measure (Friday #38): 2.5 kg goes as 2.5. A whole
        // number still goes as an integer.
        let delta = (delta * 1000.0).round() / 1000.0;
        let delta_json = if delta.fract() == 0.0 {
            json!(delta as i64)
        } else {
            json!(delta)
        };
        // Already there (an earlier attempt got through but its answer
        // didn't): skip.
        let existing = friday
            .get(
                &token,
                &format!("/api/inventory/stock_moves?reference={reference}"),
            )
            .await?;
        if !existing.as_array().is_some_and(|a| !a.is_empty()) {
            let product_id = find_or_create_product(friday, &token, name).await?;
            friday
                .post(
                    &token,
                    &format!("/api/inventory/products/{product_id}/adjust-stock"),
                    &json!({
                        "delta": delta_json,
                        // A reversal is a correction, whatever the original was.
                        "reason": if item.reverse { "manual_adjustment" } else { reason },
                        "reference": reference,
                    }),
                )
                .await?;
        }
    }

    if let (true, Some((debit, credit, cents, currency, memo))) = (to_accounting, book) {
        if !currency.eq_ignore_ascii_case(business.default_currency.trim()) {
            return Err(permanent(format!(
                "in {currency}; Accounting books {} only",
                business.default_currency
            )));
        }
        let existing = friday
            .get(
                &token,
                &format!("/api/accounting/journal_entries?source_ref={reference}"),
            )
            .await?;
        if !existing.as_array().is_some_and(|a| !a.is_empty()) {
            let (debit, credit) = if item.reverse {
                (credit, debit)
            } else {
                (debit, credit)
            };
            let accounts = accounts(friday, &token).await?;
            let date = occurred_at(&entry.event)
                .with_timezone(&business.timezone.trim().parse().unwrap_or(chrono_tz::UTC))
                .date_naive();
            let memo = if item.reverse {
                format!("Reversal: {memo}")
            } else {
                memo
            };
            friday
                .post(
                    &token,
                    "/api/accounting/journal_entries",
                    &json!({
                        "entry_date": date.to_string(),
                        "memo": memo,
                        "source": "guss",
                        "source_ref": reference,
                        "lines": [
                            {"account_id": account_id(&accounts, debit)?, "debit_cents": cents},
                            {"account_id": account_id(&accounts, credit)?, "credit_cents": cents},
                        ],
                    }),
                )
                .await?;
        }
    }
    Ok(())
}

/// The org's accounts, adding "6900 Shop expenses" the first time.
async fn accounts(friday: &FridayClient, token: &str) -> Result<Vec<Value>, Failure> {
    let list = friday.get(token, "/api/accounting/accounts").await?;
    let mut accounts = list.as_array().cloned().unwrap_or_default();
    let has = |code: &str, a: &[Value]| {
        a.iter()
            .any(|x| x.get("code").and_then(Value::as_str) == Some(code))
    };
    if !has(SHOP_EXPENSES.0, &accounts) {
        let created = friday
            .post(
                token,
                "/api/accounting/accounts",
                &json!({"code": SHOP_EXPENSES.0, "name": SHOP_EXPENSES.1, "type": "expense"}),
            )
            .await?;
        accounts.push(created);
    }
    Ok(accounts)
}

fn account_id(accounts: &[Value], code: &str) -> Result<String, Failure> {
    accounts
        .iter()
        .find(|a| a.get("code").and_then(Value::as_str) == Some(code))
        .and_then(|a| a.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or_else(|| permanent(format!("Accounting has no account {code}")))
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
    use guss_core::domain::{Expense, Sale, SourceChannel, StockUpdate};
    use guss_core::money::CurrencyAmount;
    use guss_core::people::{Actor, Role};
    use guss_store::SqliteStore;
    use uuid::Uuid;
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

    fn shop(id: Uuid) -> BusinessProfile {
        BusinessProfile {
            id,
            name: "Duka".into(),
            default_currency: "KES".into(),
            language: "en".into(),
            timezone: "Africa/Nairobi".into(),
        }
    }

    async fn friday(access: &str) -> MockServer {
        friday_with(json!({"inventory": access})).await
    }

    async fn friday_with(access: Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/guss/svc/people"))
            .and(header("authorization", format!("Bearer {KEY}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "people": [{"telegram_id": 7, "telegram_name": "Mary", "guss_role": "staff"}],
                "access": access
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
    async fn a_fraction_of_a_kilo_reaches_inventory_as_a_fraction() {
        let server = friday("write").await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let mut sugar = restock(business_id, "Sugar", 2.5);
        if let BusinessEvent::StockUpdate(su) = &mut sugar {
            su.unit = Some("kg".into());
            su.unit_factor = 1.0;
        }
        store.save_event_by(&sugar, Some(&mary)).await.unwrap();
        Mock::given(method("GET"))
            .and(path("/api/inventory/stock_moves"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/inventory/products"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"id": "p-sugar", "name": "Sugar"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/inventory/products/p-sugar/adjust-stock"))
            .and(body_partial_json(json!({"delta": 2.5, "reason": "received"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "p-sugar"})))
            .expect(1)
            .mount(&server)
            .await;
        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            1
        );
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
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            0
        );

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
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            1
        );
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
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            0
        );
        let journal = store.journal(business_id, 5).await.unwrap();
        assert_eq!(
            journal[0].sync_error.as_deref(),
            Some("Guss may not change Inventory or Accounting (Friday: Integrations, Guss)")
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
        sync_once(&store, &client, &shop(business_id))
            .await
            .unwrap();
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
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            0
        );
        let queue = store.to_sync(business_id, 10).await.unwrap();
        assert_eq!(queue.len(), 1, "kept for the next round");
        assert!(queue[0].entry.sync_error.is_some());
    }

    #[tokio::test]
    async fn expenses_and_sales_are_booked_in_accounting_as_drafts() {
        let server = friday_with(json!({"inventory": "none", "accounting": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let transport = BusinessEvent::Expense(Expense {
            id: Uuid::new_v4(),
            business_id,
            description: "transport".into(),
            amount: CurrencyAmount::new(500.0, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Telegram,
            raw_text: None,
        });
        store.save_event_by(&transport, Some(&mary)).await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/accounting/journal_entries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        // No "6900 Shop expenses" yet: Guss adds it.
        Mock::given(method("GET"))
            .and(path("/api/accounting/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "a-cash", "code": "1010"}, {"id": "a-sales", "code": "4000"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounting/accounts"))
            .and(body_partial_json(
                json!({"code": "6900", "type": "expense"}),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"id": "a-shop", "code": "6900"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounting/journal_entries"))
            .and(body_partial_json(json!({
                "source": "guss",
                "source_ref": format!("guss:{}", transport.id()),
                "memo": "Guss: transport",
                "lines": [
                    {"account_id": "a-shop", "debit_cents": 50000},
                    {"account_id": "a-cash", "credit_cents": 50000}
                ]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "je-1"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            1
        );
        assert!(store.to_sync(business_id, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn other_currencies_are_not_booked() {
        let server = friday_with(json!({"accounting": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let mut usd = sale(business_id, "phone", 1.0);
        if let BusinessEvent::Sale(s) = &mut usd {
            s.unit_price = CurrencyAmount::new(50.0, "USD");
        }
        store.save_event_by(&usd, Some(&mary)).await.unwrap();
        let client = FridayClient::new(server.uri(), KEY);
        sync_once(&store, &client, &shop(business_id))
            .await
            .unwrap();
        let journal = store.journal(business_id, 1).await.unwrap();
        assert_eq!(
            journal[0].sync_error.as_deref(),
            Some("in USD; Accounting books KES only")
        );
    }
}
