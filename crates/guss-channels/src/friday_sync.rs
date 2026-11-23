//! Sends Guss's entries to Friday, as *the person who recorded each one*
//! (their linked Friday account):
//!
//! - A sale is recorded in Friday's sales system (Invoicing), like a till
//!   sale but on the "guss" channel (owner, 2026-11-21: one place for
//!   everything). Friday then lowers stock, books it with its VAT and the
//!   money in Accounting, and lists it for eTIMS and the VAT return. A
//!   voided sale is returned there in full.
//! - Inventory: a restock raises stock, a write-off lowers it.
//! - Accounting: an expense (Dr Shop expenses, Cr Cash) and a paid restock
//!   (Dr Inventory, Cr Cash), as draft journal entries the accountant
//!   posts.
//!
//! A voided restock or expense is reversed in both. Everything is tagged
//! "guss:<entry id>" and checked first, so a retry never counts twice.
//! Nothing is lost if Friday is down: entries wait in the journal and go
//! on the next round.

use std::time::Duration;

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile, DebtPayment, Sale};
use guss_core::money::CurrencyAmount;
use guss_core::people::{Actor, Role};
use guss_core::store::{Store, StoreError, SyncItem};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::friday::{FridayClient, FridayError};
use crate::items::normalize_name;

pub const SYNC_EVERY: Duration = Duration::from_secs(30);

/// A sale's `paid_by` when Friday itself took the money (a scanned basket
/// paid by an M-Pesa prompt): Friday already has the sale, so sync skips it.
pub const PAID_THROUGH_FRIDAY: &str = "paystack";
const BATCH: i64 = 50;

/// Accounting accounts Guss books into (Friday's starter chart, plus one
/// expense account Guss adds the first time it needs it).
const CASH: &str = "1010";
const INVENTORY_ASSET: &str = "1400";
const SHOP_EXPENSES: (&str, &str) = ("6900", "Shop expenses");
/// Expenses staff paid from their own pocket, until they're paid back.
const OWED_TO_STAFF: (&str, &str) = ("2150", "Owed to staff");

/// Why an entry didn't reach Friday. `permanent` ones are shown in
/// /history and not retried.
#[derive(Debug, PartialEq)]
struct Failure {
    message: String,
    permanent: bool,
    /// Waits for someone to act (link their account, grant Guss access):
    /// retried every round, without holding up the other entries.
    waiting: bool,
}

impl From<FridayError> for Failure {
    fn from(e: FridayError) -> Self {
        Failure {
            permanent: !e.is_transient(),
            message: e.to_string(),
            waiting: false,
        }
    }
}

fn permanent(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        permanent: true,
        waiting: false,
    }
}

/// Found 2026-11-22: entries stopped for good by "not linked" or "no
/// access" stayed out of Friday after the person linked or access was
/// granted, and /history kept saying "not linked".
fn waiting(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        permanent: false,
        waiting: true,
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
                if !f.permanent && !f.waiting {
                    // Friday is down or slow: stop this round, retry later.
                    tracing::warn!(error = %f.message, "Friday sync paused");
                    break;
                }
            }
        }
    }
    Ok(done)
}

/// Pay links ("/paylink John") are checked for this many days.
const PAY_LINK_DAYS: i64 = 14;

/// How long between checks of a pay link this old: None = every round
/// (30 s) for its first 10 minutes, then every 5 minutes for a day, then
/// hourly. Each check may ask Paystack; most customers pay within minutes,
/// and with Paystack's webhook set Friday already knows (no Paystack call).
fn pay_link_gap(age: chrono::Duration) -> Option<std::time::Duration> {
    match age.num_seconds() {
        a if a < 10 * 60 => None,
        a if a < 24 * 3600 => Some(std::time::Duration::from_secs(5 * 60)),
        _ => Some(std::time::Duration::from_secs(3600)),
    }
}

/// Asks Friday about each open Paystack pay link: a note "/paylink" left
/// that no payment has closed yet. Once the customer has paid, it goes in
/// the journal as "John paid" (by M-Pesa, card or bank), so Guss's own
/// "who owes me?" agrees with Friday, which recorded it already. A link
/// paid when nothing was owed any more is closed by voiding its note.
/// Returns how many payments it recorded.
pub async fn check_pay_links<S: Store + ?Sized>(
    store: &S,
    friday: &FridayClient,
    business: &BusinessProfile,
) -> Result<usize, StoreError> {
    let journal = store.journal(business.id, 500).await?;
    let since = Utc::now() - chrono::Duration::days(PAY_LINK_DAYS);
    // Voided ones too: a payment voided in Guss is never re-recorded.
    let closed: HashSet<&str> = journal
        .iter()
        .filter_map(|e| match &e.event {
            BusinessEvent::DebtPayment(d) => d.paystack_ref.as_deref(),
            _ => None,
        })
        .collect();
    let mut recorded = 0;
    for entry in &journal {
        let BusinessEvent::Note(n) = &entry.event else {
            continue;
        };
        let Some(reference) = n.paystack_ref.as_deref() else {
            continue;
        };
        if entry.voided.is_some() || n.occurred_at < since || closed.contains(reference) {
            continue;
        }
        if let Some(gap) = pay_link_gap(Utc::now() - n.occurred_at)
            && !friday.claim_check(reference, gap).await
        {
            continue;
        }
        let token = match friday
            .token_for_actor(entry.recorded_by_id.as_deref().unwrap_or_default())
            .await
        {
            Ok(Some(t)) => t,
            _ => continue,
        };
        let reply = match friday
            .get(
                &token,
                &format!("/api/invoicing/pos/debts/paystack-link/{reference}"),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, reference, "couldn't check a pay link");
                continue;
            }
        };
        if reply["status"] != "paid" {
            continue;
        }
        let actor = Actor::new(
            entry.recorded_by_id.clone().unwrap_or_default(),
            entry.recorded_by.clone().unwrap_or_default(),
            entry.recorded_by_role.unwrap_or(Role::Staff),
        );
        let cents = reply["paid_cents"].as_i64().unwrap_or(0);
        if cents <= 0 {
            store
                .void_event(n.id, &actor, "paid through Paystack when nothing was owed")
                .await?;
            continue;
        }
        let paid_by = match reply["method"].as_str() {
            Some("bank_transfer") => "bank",
            Some(m) => m,
            None => "card",
        };
        let event = BusinessEvent::DebtPayment(DebtPayment {
            id: Uuid::new_v4(),
            business_id: business.id,
            customer: reply["customer"].as_str().unwrap_or_default().to_string(),
            amount: CurrencyAmount::new(
                cents as f64 / 100.0,
                reply["currency"]
                    .as_str()
                    .unwrap_or(business.default_currency.as_str()),
            ),
            paid_by: Some(paid_by.to_string()),
            occurred_at: Utc::now(),
            source_channel: n.source_channel,
            raw_text: Some(format!("Paystack pay link {reference}")),
            paystack_ref: Some(reference.to_string()),
        });
        store.save_event_by(&event, Some(&actor)).await?;
        recorded += 1;
    }
    Ok(recorded)
}

/// Runs `sync_once` (and the pay-link check) forever, every `SYNC_EVERY`.
pub async fn run<S: Store + ?Sized>(store: &S, friday: &FridayClient, business: BusinessProfile) {
    loop {
        match sync_once(store, friday, &business).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(entries = n, "sent to Friday"),
            Err(e) => tracing::warn!(error = %e, "Friday sync couldn't read the journal"),
        }
        match check_pay_links(store, friday, &business).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(payments = n, "pay links paid"),
            Err(e) => tracing::warn!(error = %e, "couldn't check pay links"),
        }
        tokio::time::sleep(SYNC_EVERY).await;
    }
}

/// What a journal entry means for Inventory: (item, base-unit change, reason).
fn stock_change(event: &BusinessEvent) -> Option<(&str, f64, &'static str)> {
    match event {
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
        BusinessEvent::Expense(x) => Some((
            SHOP_EXPENSES.0,
            if x.reimburse == Some(true) {
                OWED_TO_STAFF.0
            } else {
                CASH
            },
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
        BusinessEvent::DebtPayment(d) => d.occurred_at,
    }
}

async fn push(
    friday: &FridayClient,
    business: &BusinessProfile,
    item: &SyncItem,
) -> Result<(), Failure> {
    let entry = &item.entry;
    if let BusinessEvent::Sale(s) = &entry.event
        && s.paid_by.as_deref() == Some(PAID_THROUGH_FRIDAY)
    {
        // shortcut: voiding it in Guss doesn't refund it; refunds go through the till or Paystack.
        return Ok(());
    }
    let sale = match &entry.event {
        BusinessEvent::Sale(s) => Some(s),
        _ => None,
    };
    let debt = match &entry.event {
        // Paid through a Paystack link: Friday recorded it when Paystack
        // confirmed it, and a void in Guss can't take back money that
        // really came in. Nothing to send either way.
        BusinessEvent::DebtPayment(d) if d.paystack_ref.is_some() => return Ok(()),
        BusinessEvent::DebtPayment(d) => Some(d),
        _ => None,
    };
    let stock = stock_change(&entry.event);
    let book = booking(&entry.event);
    if sale.is_none() && debt.is_none() && stock.is_none() && book.is_none() {
        return Ok(());
    }
    let people = friday.people().await?;
    let to_inventory = stock.is_some() && people.can_write("inventory");
    let to_accounting = book.is_some() && people.can_write("accounting");
    if (sale.is_some() || debt.is_some()) && !people.can_write("invoicing") {
        return Err(waiting(
            "Guss may not record sales in Friday yet (Friday: Apps & integrations, Guss: Invoicing)",
        ));
    }
    // A pay-me-back expense also goes to Minions as a claim, on its own.
    let to_minions = matches!(&entry.event, BusinessEvent::Expense(x) if x.reimburse == Some(true))
        && people.can_write("minions");
    if sale.is_none() && debt.is_none() && !to_inventory && !to_accounting && !to_minions {
        return Err(waiting(
            "Guss may not change Inventory or Accounting yet (Friday: Apps & integrations, Guss)",
        ));
    }
    let who = entry
        .recorded_by
        .clone()
        .unwrap_or_else(|| "someone".into());
    let Some(token) = friday
        .token_for_actor(entry.recorded_by_id.as_deref().unwrap_or_default())
        .await?
    else {
        return Err(waiting(format!(
            "{who} isn't linked to Friday yet (send /link)"
        )));
    };
    if let Some(s) = sale {
        if s.quantity < 0.0 && item.reverse {
            // A return taken back: the customer kept it after all, so it
            // is sold again at the same price, once.
            let again = Sale {
                quantity: -s.quantity,
                paid_by: None,
                ..s.clone()
            };
            let reference = format!("guss-unreturn:{}", s.id);
            return push_sale(friday, business, &token, false, &again, reference).await;
        }
        if s.quantity < 0.0 {
            return push_return(friday, &token, s).await;
        }
        let reference = format!("guss:{}", s.id);
        return push_sale(friday, business, &token, item.reverse, s, reference).await;
    }
    if let Some(d) = debt {
        return push_debt_payment(friday, business, &token, item.reverse, d).await;
    }
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
            let (product_id, _) = find_or_create_product(friday, &token, name, true).await?;
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
            let accounts = accounts(friday, &token, [debit, credit]).await?;
            let date = occurred_at(&entry.event)
                .with_timezone(&business.timezone.trim().parse().unwrap_or(chrono_tz::UTC))
                .date_naive();
            let memo = if credit == OWED_TO_STAFF.0 || debit == OWED_TO_STAFF.0 {
                format!("{memo} (paid by {who}, to pay back)")
            } else {
                memo
            };
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

    if let BusinessEvent::Expense(x) = &entry.event
        && to_minions
    {
        if item.reverse {
            withdraw_claim(friday, &token, x).await?;
        } else {
            push_claim(friday, &token, x).await?;
        }
    }
    Ok(())
}

/// "I paid, pay me back": a Minions expense claim for the person, so it
/// goes through approval and payout like any other claim. Tagged with the
/// entry id and checked first, so a retry never claims twice.
/// A voided pay-me-back expense: its Minions claim is withdrawn while it
/// still waits. One already approved or paid stays for the approver (the
/// money may have moved).
async fn withdraw_claim(
    friday: &FridayClient,
    token: &str,
    x: &guss_core::domain::Expense,
) -> Result<(), Failure> {
    let tag = format!("(Guss {})", &x.id.to_string()[..8]);
    let mine = match friday
        .get(token, "/api/minions/expense_claims?mine=true")
        .await
    {
        Err(FridayError::Status { status: 404, .. }) => return Ok(()),
        other => other?,
    };
    let waiting = mine.as_array().into_iter().flatten().find(|c| {
        c.get("status").and_then(Value::as_str) == Some("submitted")
            && c.get("description")
                .and_then(Value::as_str)
                .is_some_and(|d| d.ends_with(&tag))
    });
    let Some(id) = waiting.and_then(|c| c.get("id")).and_then(Value::as_str) else {
        return Ok(());
    };
    match friday
        .post(
            token,
            &format!("/api/minions/expense_claims/{id}/withdraw"),
            &json!({}),
        )
        .await
    {
        // An older Minions (no withdraw), or decided meanwhile: nothing to undo here.
        Ok(_)
        | Err(FridayError::Status {
            status: 400 | 404 | 405,
            ..
        }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

async fn push_claim(
    friday: &FridayClient,
    token: &str,
    x: &guss_core::domain::Expense,
) -> Result<(), Failure> {
    let tag = format!("(Guss {})", &x.id.to_string()[..8]);
    let mine = friday
        .get(token, "/api/minions/expense_claims?mine=true")
        .await;
    let mine = match mine {
        // Not on the Minions staff list: the books still show it as owed.
        Err(FridayError::Status { status: 404, .. }) => return Ok(()),
        other => other?,
    };
    let existing = mine.as_array().into_iter().flatten().find(|c| {
        c.get("description")
            .and_then(Value::as_str)
            .is_some_and(|d| d.ends_with(&tag))
    });
    let claim = match existing {
        Some(c) => c.clone(),
        None => {
            friday
                .post(
                    token,
                    "/api/minions/expense_claims",
                    &json!({
                        "description": format!("{} {tag}", x.description),
                        "amount_cents": (x.amount.major_units() * 100.0).round() as i64,
                        "currency": x.amount.currency,
                        "reimburse": true,
                    }),
                )
                .await?
        }
    };
    // The receipt photo sent with it, if Guss still has it.
    if let (Some(photo), Some(id)) = (
        crate::receipts::take(x.id),
        claim.get("id").and_then(Value::as_str),
    ) {
        let path = format!("/api/minions/expense_claims/{id}/receipt");
        match friday
            .put_bytes(token, &path, "image/jpeg", photo.clone())
            .await
        {
            Ok(_) => {}
            // An older Minions, or the claim was already decided: the claim stands without it.
            Err(FridayError::Status { status, .. }) if status < 500 && status != 429 => {
                tracing::warn!(status, "Minions refused a receipt photo");
            }
            Err(e) => {
                crate::receipts::put_back(x.id, photo);
                return Err(e.into());
            }
        }
    }
    Ok(())
}

/// A sale goes to Friday's sales system on the "guss" channel; its void
/// is a full return there. Friday records a sale once per
/// client_sale_id, so a retry never sells twice.
async fn push_sale(
    friday: &FridayClient,
    business: &BusinessProfile,
    token: &str,
    reverse: bool,
    s: &Sale,
    reference: String,
) -> Result<(), Failure> {
    let total = s.total();
    if !total
        .currency
        .eq_ignore_ascii_case(business.default_currency.trim())
    {
        return Err(permanent(format!(
            "in {}; Friday records sales in {} only",
            total.currency, business.default_currency
        )));
    }
    if reverse {
        // Find the sale Friday recorded and return it in full. Never
        // recorded, or already returned: nothing left to undo.
        let found = match friday
            .get(token, &format!("/api/invoicing/pos/sales/{reference}"))
            .await
        {
            Ok(v) => v,
            Err(FridayError::Status { status: 404, .. }) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if found.get("status").and_then(Value::as_str) == Some("void") {
            return Ok(());
        }
        let number = found
            .get("number")
            .and_then(Value::as_str)
            .ok_or_else(|| permanent("Friday sent a sale without a number"))?;
        return match friday
            .post(
                token,
                &format!("/api/invoicing/pos/sales/{number}/return"),
                &json!({}),
            )
            .await
        {
            Ok(_) | Err(FridayError::Status { status: 400, .. }) => Ok(()),
            Err(e) => Err(e.into()),
        };
    }
    let (product_id, tax_code) = find_or_create_product(friday, token, &s.item_name, false).await?;
    let mut line = json!({
        "description": s.item_name,
        "quantity": (s.quantity * 1000.0).round() / 1000.0,
        "unit_price_cents": s.unit_price.amount.minor_units,
        "product_id": product_id,
    });
    if !tax_code.is_empty() {
        line["tax_code"] = json!(tax_code);
    }
    if let Some(unit) = &s.unit {
        // "2 crates": Friday lowers stock by 2 x 24 bottles.
        line["uom"] = json!(unit);
        line["uom_factor"] = json!(s.unit_factor);
    }
    let mut body = json!({
        "client_sale_id": reference,
        "channel": "guss",
        "currency": total.currency,
        // As said ("paid by mpesa"); unsaid counts as cash.
        "method": s.paid_by.as_deref().unwrap_or("cash"),
        "amount_cents": total.amount.minor_units,
        "created_at_ms": s.occurred_at.timestamp_millis(),
        "lines": [line],
    });
    if let Some(who) = &s.credit_to {
        // Unpaid against the customer until a DebtPayment.
        body["on_credit"] = json!(true);
        body["customer_name"] = json!(who);
    }
    friday
        .post(token, "/api/invoicing/pos/sales", &body)
        .await?;
    Ok(())
}

/// A return ("returned 1 rice") gives back part of the Guss sale it
/// names, at that sale's price; Friday refunds once per return id.
async fn push_return(friday: &FridayClient, token: &str, r: &Sale) -> Result<(), Failure> {
    let original = r
        .returns
        .ok_or_else(|| permanent("this return doesn't name the sale it gives back"))?;
    let found = match friday
        .get(token, &format!("/api/invoicing/pos/sales/guss:{original}"))
        .await
    {
        Ok(v) => v,
        Err(FridayError::Status { status: 404, .. }) => {
            return Err(permanent("the sale it gives back isn't in Friday"));
        }
        Err(e) => return Err(e.into()),
    };
    let number = found
        .get("number")
        .and_then(Value::as_str)
        .ok_or_else(|| permanent("Friday sent a sale without a number"))?;
    let line = found
        .get("lines")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .ok_or_else(|| permanent("Friday sent a sale without lines"))?;
    let line_id = line
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| permanent("Friday sent a sale line without an id"))?;
    // Friday counts the line in the unit it was sold in.
    let factor = line
        .get("uom_factor")
        .and_then(|f| {
            f.as_f64()
                .or_else(|| f.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|f| *f > 0.0)
        .unwrap_or(1.0);
    let quantity = ((r.base_quantity().abs() / factor) * 1000.0).round() / 1000.0;
    match friday
        .post(
            token,
            &format!("/api/invoicing/pos/sales/{number}/return"),
            &json!({
                "client_return_id": format!("guss:{}", r.id),
                "lines": [{"line_id": line_id, "quantity": quantity}],
            }),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(FridayError::Status {
            status: 400 | 409 | 422,
            body,
        }) => Err(permanent(format!("Friday refused the return: {body}"))),
        Err(e) => Err(e.into()),
    }
}

/// "John paid 500": settles John's oldest unpaid credit sales in Friday,
/// once per payment id. Voiding it takes the payment back there: those
/// sales are owed again.
async fn push_debt_payment(
    friday: &FridayClient,
    business: &BusinessProfile,
    token: &str,
    reverse: bool,
    d: &DebtPayment,
) -> Result<(), Failure> {
    let reference = format!("guss:{}", d.id);
    if reverse {
        return match friday
            .post(
                token,
                &format!("/api/invoicing/pos/debts/payments/{reference}/reverse"),
                &json!({}),
            )
            .await
        {
            // Never recorded there: nothing to take back.
            Ok(_) | Err(FridayError::Status { status: 404, .. }) => Ok(()),
            Err(FridayError::Status { status: 409, body }) => {
                Err(permanent(format!("Friday refused to take it back: {body}")))
            }
            Err(e) => Err(e.into()),
        };
    }
    if !d
        .amount
        .currency
        .eq_ignore_ascii_case(business.default_currency.trim())
    {
        return Err(permanent(format!(
            "in {}; Friday records payments in {} only",
            d.amount.currency, business.default_currency
        )));
    }
    match friday
        .post(
            token,
            "/api/invoicing/pos/debts/payments",
            &json!({
                "customer_name": d.customer,
                "amount_cents": d.amount.amount.minor_units,
                "method": d.paid_by.as_deref().unwrap_or("cash"),
                "client_payment_id": reference,
            }),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(FridayError::Status {
            status: 404 | 409 | 422,
            body,
        }) => Err(permanent(format!("Friday refused the payment: {body}"))),
        Err(e) => Err(e.into()),
    }
}

/// The org's accounts, adding "6900 Shop expenses" or "2150 Owed to
/// staff" the first time an entry in `used` needs it.
async fn accounts(
    friday: &FridayClient,
    token: &str,
    used: [&str; 2],
) -> Result<Vec<Value>, Failure> {
    let list = friday.get(token, "/api/accounting/accounts").await?;
    let mut accounts = list.as_array().cloned().unwrap_or_default();
    let has = |code: &str, a: &[Value]| {
        a.iter()
            .any(|x| x.get("code").and_then(Value::as_str) == Some(code))
    };
    for (code, name, kind) in [
        (SHOP_EXPENSES.0, SHOP_EXPENSES.1, "expense"),
        (OWED_TO_STAFF.0, OWED_TO_STAFF.1, "liability"),
    ] {
        if used.contains(&code) && !has(code, &accounts) {
            let created = friday
                .post(
                    token,
                    "/api/accounting/accounts",
                    &json!({"code": code, "name": name, "type": kind}),
                )
                .await?;
            accounts.push(created);
        }
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
/// same item), created if Friday doesn't have it yet: its id and its tax
/// type ("" for the business default).
/// `track`: a product Guss makes for a restock counts its stock; one it
/// makes for a sale doesn't (Guss can't know the shelf), so its sales
/// never drive a count negative (Friday plan 20.2 #2).
async fn find_or_create_product(
    friday: &FridayClient,
    token: &str,
    name: &str,
    track: bool,
) -> Result<(String, String), Failure> {
    let wanted = normalize_name(name);
    let products = friday.products(token).await?;
    if let Some(found) = products.as_array().into_iter().flatten().find(|p| {
        p.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| normalize_name(n) == wanted)
    }) {
        let tax_code = found
            .get("tax_code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return found
            .get("id")
            .and_then(Value::as_str)
            .map(|id| (id.to_string(), tax_code))
            .ok_or_else(|| permanent("Friday sent a product without an id"));
    }
    let created = friday
        .post(
            token,
            "/api/inventory/products",
            &json!({"name": name, "kind": "product", "track_inventory": track}),
        )
        .await?;
    friday.forget_products().await;
    created
        .get("id")
        .and_then(Value::as_str)
        .map(|id| (id.to_string(), String::new()))
        .ok_or_else(|| permanent("Friday created a product without an id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use guss_core::domain::{DebtPayment, Expense, Sale, SourceChannel, StockUpdate};
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
            paid_by: None,
            credit_to: None,
            returns: None,
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
    async fn a_paid_pay_link_is_recorded_once_and_never_sent_back() {
        use guss_core::domain::Note;
        use wiremock::matchers::path_regex;

        let server = friday_with(json!({"invoicing": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let reference = "org.debt-john.0a1b2c3d";
        let note = BusinessEvent::Note(Note {
            id: Uuid::new_v4(),
            business_id,
            text: "Paystack pay link for John: 1,500.00 KES".into(),
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Telegram,
            paystack_ref: Some(reference.into()),
        });
        store.save_event_by(&note, Some(&mary)).await.unwrap();
        let client = FridayClient::new(server.uri(), KEY);
        let check = format!("/api/invoicing/pos/debts/paystack-link/{reference}");

        // Not paid yet: nothing recorded.
        let waiting = Mock::given(method("GET"))
            .and(path(check.as_str()))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "abandoned"})))
            .mount_as_scoped(&server)
            .await;
        assert_eq!(
            check_pay_links(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            0
        );
        drop(waiting);

        Mock::given(method("GET"))
            .and(path(check.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "paid", "customer": "John", "paid_cents": 150000,
                "method": "mpesa", "currency": "KES"
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Friday already has it: no debt payment may be posted.
        Mock::given(method("POST"))
            .and(path_regex("^/api/invoicing/pos/debts/payments"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        assert_eq!(
            check_pay_links(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            1
        );
        // Closed now: checked no more.
        assert_eq!(
            check_pay_links(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            0
        );
        let journal = store.journal(business_id, 10).await.unwrap();
        let paid = journal
            .iter()
            .find_map(|e| match &e.event {
                BusinessEvent::DebtPayment(d) => Some(d.clone()),
                _ => None,
            })
            .expect("John's payment is in the journal");
        assert_eq!(paid.customer, "John");
        assert_eq!(paid.amount, CurrencyAmount::new(1500.0, "KES"));
        assert_eq!(paid.paid_by.as_deref(), Some("mpesa"));
        assert_eq!(paid.paystack_ref.as_deref(), Some(reference));

        // Syncing marks it done without sending it.
        sync_once(&store, &client, &shop(business_id))
            .await
            .unwrap();
        assert!(store.to_sync(business_id, 10).await.unwrap().is_empty());
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
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"id": "p-sugar", "name": "Sugar"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/inventory/products/p-sugar/adjust-stock"))
            .and(body_partial_json(
                json!({"delta": 2.5, "reason": "received"}),
            ))
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
            Some(
                "Guss may not record sales in Friday yet (Friday: Apps & integrations, Guss: Invoicing)"
            )
        );
        // Still waiting: it goes as soon as access is granted.
        assert_eq!(store.to_sync(business_id, 10).await.unwrap().len(), 1);

        // Write access, but the author isn't linked.
        let server = friday_with(json!({"invoicing": "write"})).await;
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
            Some("Sam isn't linked to Friday yet (send /link)")
        );
        assert!(
            store
                .to_sync(business_id, 10)
                .await
                .unwrap()
                .iter()
                .any(|i| i.entry.recorded_by.as_deref() == Some("Sam"))
        );
    }

    #[tokio::test]
    async fn a_link_from_friday_is_redeemed_with_the_service_key() {
        let server = friday("write").await;
        Mock::given(method("POST"))
            .and(path("/guss/svc/redeem"))
            .and(header("authorization", format!("Bearer {KEY}").as_str()))
            .and(body_partial_json(
                json!({"code": "AbC_9", "telegram_id": 7}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"guss_role": "owner"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/guss/svc/bot"))
            .and(body_partial_json(json!({"username": "DukaGussBot"})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(client.redeem("AbC_9", 7, "Fire").await.unwrap(), "owner");
        client.report_bot("DukaGussBot").await.unwrap();
    }

    #[tokio::test]
    async fn widget_entries_act_as_the_friday_user_who_typed_them() {
        let server = friday("write").await;
        Mock::given(method("POST"))
            .and(path("/guss/svc/token"))
            .and(body_partial_json(json!({"user_id": "u-amina"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"token": "jwt-amina", "expires_in": 600})),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(
            client
                .token_for_actor("friday:u-amina")
                .await
                .unwrap()
                .as_deref(),
            Some("jwt-amina")
        );
        assert_eq!(
            client
                .token_for_actor("telegram:7")
                .await
                .unwrap()
                .as_deref(),
            Some(TOKEN)
        );
        assert_eq!(client.token_for_actor("web").await.unwrap(), None);
        assert_eq!(client.token_for_actor("friday:").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_entry_stopped_for_good_can_be_sent_again() {
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let soda = sale(business_id, "soda", 1.0);
        store.save_event_by(&soda, Some(&mary)).await.unwrap();
        store
            .mark_sync_failed(soda.id(), "old reason", true)
            .await
            .unwrap();
        assert!(store.to_sync(business_id, 10).await.unwrap().is_empty());
        let number = store.journal(business_id, 1).await.unwrap()[0].number;

        assert!(store.resend(business_id, number).await.unwrap());
        assert_eq!(store.to_sync(business_id, 10).await.unwrap().len(), 1);
        assert_eq!(
            store.journal(business_id, 1).await.unwrap()[0].sync_error,
            None
        );
        // Only stopped entries: a second resend changes nothing.
        assert!(!store.resend(business_id, number).await.unwrap());
    }

    #[tokio::test]
    async fn a_sale_friday_already_took_the_money_for_is_not_sent_again() {
        // No sales mock: posting it would fail the round.
        let server = friday_with(json!({"invoicing": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let BusinessEvent::Sale(mut s) = sale(business_id, "soda", 2.0) else {
            unreachable!()
        };
        s.paid_by = Some(PAID_THROUGH_FRIDAY.into());
        store
            .save_event_by(&BusinessEvent::Sale(s), Some(&mary))
            .await
            .unwrap();
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
    async fn money_staff_paid_is_owed_to_them_and_claimed_in_minions() {
        let server = friday_with(json!({"accounting": "write", "minions": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let id = Uuid::new_v4();
        let fuel = BusinessEvent::Expense(Expense {
            id,
            business_id,
            description: "fuel".into(),
            amount: CurrencyAmount::new(2500.0, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Telegram,
            raw_text: None,
            reimburse: Some(true),
        });
        store.save_event_by(&fuel, Some(&mary)).await.unwrap();
        // Mary sent the receipt photo with it.
        crate::receipts::hold("chat-mary", vec![0xFF, 0xD8, 0xFF]);
        crate::receipts::attach("chat-mary", &[id]);

        Mock::given(method("GET"))
            .and(path("/api/accounting/journal_entries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/accounting/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "a-cash", "code": "1010"}, {"id": "a-shop", "code": "6900"}
            ])))
            .mount(&server)
            .await;
        // Owed to staff, not cash: added the first time.
        Mock::given(method("POST"))
            .and(path("/api/accounting/accounts"))
            .and(body_partial_json(
                json!({"code": "2150", "type": "liability"}),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"id": "a-owed", "code": "2150"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounting/journal_entries"))
            .and(body_partial_json(json!({
                "memo": "Guss: fuel (paid by Mary, to pay back)",
                "lines": [
                    {"account_id": "a-shop", "debit_cents": 250000},
                    {"account_id": "a-owed", "credit_cents": 250000}
                ]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "je-1"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/minions/expense_claims"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/minions/expense_claims"))
            .and(body_partial_json(json!({
                "description": format!("fuel (Guss {})", &id.to_string()[..8]),
                "amount_cents": 250000, "currency": "KES", "reimburse": true
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "c-1"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/minions/expense_claims/c-1/receipt"))
            .and(header("content-type", "image/jpeg"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "c-1"})))
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
    async fn voiding_a_pay_me_back_expense_withdraws_its_waiting_claim() {
        let server = friday_with(json!({"minions": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let id = Uuid::new_v4();
        let fuel = BusinessEvent::Expense(Expense {
            id,
            business_id,
            description: "fuel".into(),
            amount: CurrencyAmount::new(500.0, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Telegram,
            raw_text: None,
            reimburse: Some(true),
        });
        store.save_event_by(&fuel, Some(&mary)).await.unwrap();
        let tag = format!("fuel (Guss {})", &id.to_string()[..8]);
        Mock::given(method("GET"))
            .and(path("/api/minions/expense_claims"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!([{"id": "c-9", "description": tag, "status": "submitted"}]),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/minions/expense_claims"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "c-9"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/minions/expense_claims/c-9/withdraw"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "c-9"})))
            .expect(1)
            .mount(&server)
            .await;
        let client = FridayClient::new(server.uri(), KEY);
        sync_once(&store, &client, &shop(business_id))
            .await
            .unwrap();
        let number = store.journal(business_id, 1).await.unwrap()[0].number;
        let entry = store
            .journal_entry(business_id, number)
            .await
            .unwrap()
            .unwrap();
        store
            .void_event(entry.event.id(), &mary, "wrong")
            .await
            .unwrap();
        sync_once(&store, &client, &shop(business_id))
            .await
            .unwrap();
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
            reimburse: None,
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
        let server = friday_with(json!({"invoicing": "write"})).await;
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
            Some("in USD; Friday records sales in KES only")
        );
    }

    #[tokio::test]
    async fn credit_sales_returns_and_debt_payments_reach_friday() {
        let server = friday_with(json!({"invoicing": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);

        let mut credit = sale(business_id, "Sugar", 2.0);
        if let BusinessEvent::Sale(s) = &mut credit {
            s.credit_to = Some("John".into());
        }
        let sold = sale(business_id, "Rice", 3.0);
        let mut back = sale(business_id, "Rice", -1.0);
        if let BusinessEvent::Sale(s) = &mut back {
            s.returns = Some(sold.id());
        }
        let paid = BusinessEvent::DebtPayment(DebtPayment {
            id: Uuid::new_v4(),
            business_id,
            customer: "John".into(),
            amount: CurrencyAmount::new(100.0, "KES"),
            paid_by: Some("mpesa".into()),
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Telegram,
            raw_text: None,
            paystack_ref: None,
        });
        for e in [&credit, &sold, &back, &paid] {
            store.save_event_by(e, Some(&mary)).await.unwrap();
        }

        Mock::given(method("GET"))
            .and(path("/api/inventory/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        // A product Guss makes for a sale doesn't track stock.
        Mock::given(method("POST"))
            .and(path("/api/inventory/products"))
            .and(body_partial_json(json!({"track_inventory": false})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "p-1"})))
            .mount(&server)
            .await;
        // On credit: unpaid against John.
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/sales"))
            .and(body_partial_json(
                json!({"on_credit": true, "customer_name": "John"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": "POS-C"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/sales"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": "POS-R"})))
            .mount(&server)
            .await;
        // The return finds the rice sale and gives back one, once.
        Mock::given(method("GET"))
            .and(path(format!("/api/invoicing/pos/sales/guss:{}", sold.id())))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "number": "POS-R", "status": "paid",
                "lines": [{"id": "line-1", "quantity": "3.000", "uom_factor": null}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/sales/POS-R/return"))
            .and(body_partial_json(json!({
                "client_return_id": format!("guss:{}", back.id()),
                "lines": [{"line_id": "line-1", "quantity": 1.0}]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": "RET-1"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/debts/payments"))
            .and(body_partial_json(json!({
                "customer_name": "John", "amount_cents": 10000, "method": "mpesa",
                "client_payment_id": format!("guss:{}", paid.id())
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"owed_cents": 6000})))
            .expect(1)
            .mount(&server)
            .await;

        let client = FridayClient::new(server.uri(), KEY);
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            4
        );

        // Both voided afterwards: the rice is sold again (the customer kept
        // it), and the payment is taken back so John owes it again.
        let boss = Actor::new("telegram:1", "Boss", Role::Owner);
        store.void_event(back.id(), &boss, "kept it").await.unwrap();
        store
            .void_event(paid.id(), &boss, "wrong person")
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/sales"))
            .and(body_partial_json(json!({
                "client_sale_id": format!("guss-unreturn:{}", back.id()),
                "lines": [{"quantity": 1.0}]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": "POS-U"})))
            .with_priority(1) // ahead of the catch-all sale mock above
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/invoicing/pos/debts/payments/guss:{}/reverse",
                paid.id()
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"reopened": ["POS-C"]})))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            sync_once(&store, &client, &shop(business_id))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn a_sale_goes_to_fridays_sales_system_once_and_its_void_returns_it() {
        // Invoicing write only: the sale never touches stock or the books
        // directly; Friday does both from the one sale.
        let server = friday_with(json!({"invoicing": "write"})).await;
        let business_id = Uuid::new_v4();
        let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
        let mary = Actor::new("telegram:7", "Mary", Role::Staff);
        let mut crates = sale(business_id, "Sodas", 2.0);
        if let BusinessEvent::Sale(s) = &mut crates {
            s.unit = Some("crate".into());
            s.unit_factor = 24.0;
            s.unit_price = CurrencyAmount::new(1200.0, "KES");
            s.paid_by = Some("mpesa".into());
        }
        store.save_event_by(&crates, Some(&mary)).await.unwrap();
        let reference = format!("guss:{}", crates.id());

        Mock::given(method("GET"))
            .and(path("/api/inventory/products"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"id": "p-soda", "name": "Soda", "tax_code": "B"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/sales"))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(body_partial_json(json!({
                "client_sale_id": reference,
                "channel": "guss",
                "currency": "KES",
                // Said "paid by mpesa": Friday books it to M-Pesa, not cash.
                "method": "mpesa",
                "amount_cents": 240000,
                "lines": [{
                    "description": "Sodas", "quantity": 2.0, "unit_price_cents": 120000,
                    "product_id": "p-soda", "tax_code": "B", "uom": "crate", "uom_factor": 24.0
                }]
            })))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({"number": "POS-1", "status": "paid"})),
            )
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

        // Voided: Friday finds the sale by Guss's reference and returns it.
        let boss = Actor::new("telegram:1", "Boss", Role::Owner);
        store.void_event(crates.id(), &boss, "wrong").await.unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/api/invoicing/pos/sales/{reference}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"number": "POS-1", "status": "paid"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/invoicing/pos/sales/POS-1/return"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": "RET-1"})))
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
    async fn pay_links_are_checked_less_often_as_they_age() {
        let mins = |m: i64| chrono::Duration::minutes(m);
        assert_eq!(pay_link_gap(mins(9)), None, "every round for 10 minutes");
        assert_eq!(
            pay_link_gap(mins(30)),
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(
            pay_link_gap(mins(25 * 60)),
            Some(std::time::Duration::from_secs(3600))
        );
        let client = FridayClient::new("http://unused", KEY);
        let five = std::time::Duration::from_secs(300);
        assert!(client.claim_check("ref", five).await);
        assert!(
            !client.claim_check("ref", five).await,
            "checked a moment ago"
        );
        assert!(client.claim_check("other", five).await);
    }

    #[tokio::test]
    async fn the_product_list_is_fetched_once_per_round_and_again_after_adding_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/inventory/products"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"id": "p1", "name": "Rice"}])),
            )
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/inventory/products"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "p2"})))
            .mount(&server)
            .await;
        let client = FridayClient::new(server.uri(), KEY);
        for _ in 0..3 {
            assert_eq!(
                find_or_create_product(&client, TOKEN, "rice", true)
                    .await
                    .unwrap()
                    .0,
                "p1"
            );
        }
        assert_eq!(
            find_or_create_product(&client, TOKEN, "Sugar", true)
                .await
                .unwrap()
                .0,
            "p2"
        );
        // Added Sugar: the next lookup reads the list again.
        find_or_create_product(&client, TOKEN, "rice", true)
            .await
            .unwrap();
    }
}
