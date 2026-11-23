//! Reports with Friday linked (plan 20.2 #5): the tills' sales and
//! Friday's stock count too, so "what did we make this week" includes the
//! POS. Guss's own sales already reach Friday as channel "guss", so only
//! channel "till" is read here (never counted twice).

use chrono::{DateTime, Days, NaiveDateTime, Utc};
use guss_core::domain::{BusinessEvent, Sale, SourceChannel};
use guss_core::money::CurrencyAmount;
use guss_core::store::StockLevel;
use serde::Deserialize;
use uuid::Uuid;

use crate::friday::{FridayClient, FridayError};

#[derive(Deserialize)]
struct TillSale {
    status: String,
    issued_at: String,
    currency: String,
    total_cents: i64,
    #[serde(default)]
    lines: Vec<TillLine>,
}

#[derive(Deserialize)]
struct TillLine {
    description: String,
    quantity: f64,
    /// Missing from Friday before this field was added: then the sale's
    /// total is split by quantity.
    total_cents: Option<i64>,
}

#[derive(Deserialize)]
struct Product {
    name: String,
    #[serde(default)]
    variant_name: String,
    on_hand_qty: f64,
    reorder_point: f64,
    track_inventory: bool,
}

/// What one report needs from Friday, read at the same time.
pub struct FromFriday {
    /// The tills' sales; Err keeps the report to Guss's own numbers.
    pub till: Result<Vec<BusinessEvent>, FridayError>,
    /// Paid on invoices, per currency; empty if the person can't read
    /// invoices.
    pub invoices_paid: Vec<CurrencyAmount>,
    /// Friday's low stock; None leaves Guss's own.
    pub low_stock: Option<Vec<StockLevel>>,
}

pub async fn read(
    friday: &FridayClient,
    token: &str,
    business_id: Uuid,
    since: DateTime<Utc>,
) -> FromFriday {
    let (till, invoices, stock) = tokio::join!(
        till_sales(friday, token, business_id, since),
        invoices_paid(friday, token, since),
        low_stock(friday, token),
    );
    FromFriday {
        till,
        invoices_paid: invoices.unwrap_or_else(|e| {
            if !matches!(e, FridayError::Forbidden(_)) {
                tracing::warn!(error = %e, "invoice payments not read for the report");
            }
            Vec::new()
        }),
        low_stock: stock.ok(),
    }
}

/// `start=..&end=..` for Friday, in UTC with no `+` to escape.
fn period(since: DateTime<Utc>) -> String {
    let until = Utc::now() + chrono::Duration::hours(1);
    format!(
        "start={}&end={}",
        since.format("%Y-%m-%dT%H:%M:%SZ"),
        until.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

fn unreadable(what: &str, e: serde_json::Error) -> FridayError {
    FridayError::Status {
        status: 200,
        body: format!("{what}: {e}"),
    }
}

/// An older Friday without the endpoint.
fn missing(e: &FridayError) -> bool {
    matches!(
        e,
        FridayError::Status {
            status: 404 | 405,
            ..
        }
    )
}

/// The tills' paid sales (and returns, negative) since `since`, as the
/// person behind `token` may see them (Friday scopes staff to their own):
/// one request, added up by Friday per item.
pub async fn till_sales(
    friday: &FridayClient,
    token: &str,
    business_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<BusinessEvent>, FridayError> {
    let path = format!(
        "/api/invoicing/pos/sales-summary?{}&channel=till",
        period(since)
    );
    match friday.get(token, &path).await {
        Ok(rows) => {
            let rows: Vec<Summary> =
                serde_json::from_value(rows).map_err(|e| unreadable("till sales", e))?;
            Ok(rows
                .into_iter()
                .flat_map(|r| split(r.total_cents, &r.currency, r.lines, Utc::now(), business_id))
                .collect())
        }
        Err(e) if missing(&e) => till_sales_by_day(friday, token, business_id, since).await,
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
struct Summary {
    currency: String,
    total_cents: i64,
    #[serde(default)]
    lines: Vec<TillLine>,
}

#[derive(Deserialize)]
struct Paid {
    currency: String,
    total_cents: i64,
}

async fn invoices_paid(
    friday: &FridayClient,
    token: &str,
    since: DateTime<Utc>,
) -> Result<Vec<CurrencyAmount>, FridayError> {
    let path = format!("/api/invoicing/reports/payments-received?{}", period(since));
    let rows = match friday.get(token, &path).await {
        Err(e) if missing(&e) => return Ok(Vec::new()),
        other => other?,
    };
    let rows: Vec<Paid> = serde_json::from_value(rows).map_err(|e| unreadable("payments", e))?;
    Ok(rows
        .into_iter()
        .map(|p| CurrencyAmount::new(p.total_cents as f64 / 100.0, p.currency.as_str()))
        .collect())
}

/// Friday before /pos/sales-summary: a day at a time.
async fn till_sales_by_day(
    friday: &FridayClient,
    token: &str,
    business_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<BusinessEvent>, FridayError> {
    let mut events = Vec::new();
    let mut day = since.date_naive();
    while day <= Utc::now().date_naive() {
        let rows = friday
            .get(
                token,
                &format!("/api/invoicing/pos/sales?day={day}&channel=till"),
            )
            .await?;
        let rows: Vec<TillSale> =
            serde_json::from_value(rows).map_err(|e| unreadable("till sales", e))?;
        events.extend(sales_from(rows, business_id, since));
        day = day + Days::new(1);
    }
    Ok(events)
}

fn issued(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|n| n.and_utc())
        })
}

fn sales_from(rows: Vec<TillSale>, business_id: Uuid, since: DateTime<Utc>) -> Vec<BusinessEvent> {
    let mut out = Vec::new();
    for row in rows {
        let Some(at) = issued(&row.issued_at) else {
            tracing::warn!(at = %row.issued_at, "till sale with an unreadable time, left out of the report");
            continue;
        };
        if row.status == "paid" && at >= since {
            out.extend(split(
                row.total_cents,
                &row.currency,
                row.lines,
                at,
                business_id,
            ));
        }
    }
    out
}

/// One sale per line. `total` (after discounts, with VAT) is split over
/// the lines by their totals (signed, so returns stay negative), the last
/// line taking the remainder, so the money adds up to the cent.
fn split(
    total: i64,
    currency: &str,
    mut lines: Vec<TillLine>,
    at: DateTime<Utc>,
    business_id: Uuid,
) -> Vec<BusinessEvent> {
    let by_total = lines.iter().any(|l| l.total_cents.unwrap_or(0) != 0);
    let weights: Vec<i128> = lines
        .iter()
        .map(|l| {
            if by_total {
                l.total_cents.unwrap_or(0) as i128
            } else {
                (l.quantity * 1000.0).round() as i128
            }
        })
        .collect();
    let sum: i128 = weights.iter().sum();
    if sum == 0 {
        // shortcut: lines that cancel out (all returned) report as one line; item counts lost then.
        lines = vec![TillLine {
            description: "Till sales".into(),
            quantity: 1.0,
            total_cents: None,
        }];
    }
    let n = lines.len();
    let mut left = total;
    let mut out = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        let share = if i + 1 == n || sum == 0 {
            left
        } else {
            (total as i128 * weights[i] / sum) as i64
        };
        left -= share;
        out.push(BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id,
            item_name: line.description,
            // One "unit" of `quantity` items priced at the line's
            // share: the money stays exact, the count stays right.
            quantity: 1.0,
            unit: None,
            unit_factor: line.quantity,
            unit_price: CurrencyAmount::new(share as f64 / 100.0, currency),
            category: None,
            occurred_at: at,
            recurring: None,
            source_channel: SourceChannel::Web,
            paid_by: None,
            credit_to: None,
            returns: None,
            raw_text: None,
        }));
    }
    out
}

/// Friday's stock: tracked products at or under their reorder point.
pub async fn low_stock(friday: &FridayClient, token: &str) -> Result<Vec<StockLevel>, FridayError> {
    let rows = friday.products(token).await?;
    let products: Vec<Product> =
        Vec::<Product>::deserialize(rows.as_ref()).map_err(|e| FridayError::Status {
            status: 200,
            body: format!("products: {e}"),
        })?;
    Ok(products
        .into_iter()
        .filter(|p| p.track_inventory && p.reorder_point > 0.0 && p.on_hand_qty <= p.reorder_point)
        .map(|p| StockLevel {
            item_name: if p.variant_name.is_empty() {
                p.name
            } else {
                format!("{} {}", p.name, p.variant_name)
            },
            current_quantity: p.on_hand_qty,
            reorder_threshold: Some(p.reorder_point),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows(v: serde_json::Value) -> Vec<TillSale> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_till_sale_splits_its_total_exactly_and_skips_voids_and_old_sales() {
        let since = Utc::now() - chrono::Duration::days(1);
        let now = Utc::now().to_rfc3339();
        let sales = sales_from(
            rows(json!([
                {"status": "paid", "issued_at": now, "currency": "KES", "total_cents": 100000,
                 "lines": [{"description": "Rice", "quantity": 3.0, "total_cents": 66667},
                           {"description": "Oil", "quantity": 1.0, "total_cents": 33334}]},
                {"status": "void", "issued_at": now, "currency": "KES", "total_cents": 5000, "lines": []},
                {"status": "paid", "issued_at": "2020-01-01T00:00:00+00:00", "currency": "KES", "total_cents": 5000, "lines": []},
                // Old Friday: no line totals, a return.
                {"status": "paid", "issued_at": "2099-01-01T00:00:00", "currency": "KES", "total_cents": -2000,
                 "lines": [{"description": "Soda", "quantity": -2.0}]}
            ])),
            Uuid::nil(),
            since,
        );
        let got: Vec<(String, f64, i64)> = sales
            .iter()
            .map(|e| match e {
                BusinessEvent::Sale(s) => (
                    s.item_name.clone(),
                    s.base_quantity(),
                    s.total().amount.minor_units,
                ),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("Rice".into(), 3.0, 66666),
                ("Oil".into(), 1.0, 33334),
                ("Soda".into(), -2.0, -2000),
            ]
        );
    }

    #[test]
    fn returns_in_a_period_stay_negative_and_discounts_spread() {
        let lines: Vec<TillLine> = serde_json::from_value(json!([
            {"description": "Rice", "quantity": 4.0, "total_cents": 1000},
            {"description": "Soda", "quantity": -1.0, "total_cents": -200}
        ]))
        .unwrap();
        // 800 of goods less 80 of discounts = 720.
        let got: Vec<(String, f64, i64)> = split(720, "KES", lines, Utc::now(), Uuid::nil())
            .iter()
            .map(|e| match e {
                BusinessEvent::Sale(s) => (
                    s.item_name.clone(),
                    s.base_quantity(),
                    s.total().amount.minor_units,
                ),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            got,
            vec![("Rice".into(), 4.0, 900), ("Soda".into(), -1.0, -180)]
        );
    }
}
