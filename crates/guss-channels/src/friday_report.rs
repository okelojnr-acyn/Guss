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

/// The tills' paid sales (and returns, negative) since `since`, as the
/// person behind `token` may see them (Friday scopes staff to their own).
pub async fn till_sales(
    friday: &FridayClient,
    token: &str,
    business_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<BusinessEvent>, FridayError> {
    let mut events = Vec::new();
    let mut day = since.date_naive();
    // shortcut: one request per UTC day (31 for a month); add a date-range endpoint to Friday if reports get slow.
    while day <= Utc::now().date_naive() {
        let rows = friday
            .get(
                token,
                &format!("/api/invoicing/pos/sales?day={day}&channel=till"),
            )
            .await?;
        let rows: Vec<TillSale> =
            serde_json::from_value(rows).map_err(|e| FridayError::Status {
                status: 200,
                body: format!("till sales: {e}"),
            })?;
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

/// One sale per line. The sale's total (after discounts, with VAT) is
/// split over its lines by their totals, the last line taking the
/// remainder, so the money adds up to the cent.
fn sales_from(rows: Vec<TillSale>, business_id: Uuid, since: DateTime<Utc>) -> Vec<BusinessEvent> {
    let mut out = Vec::new();
    for row in rows {
        let Some(at) = issued(&row.issued_at) else {
            tracing::warn!(at = %row.issued_at, "till sale with an unreadable time, left out of the report");
            continue;
        };
        if row.status != "paid" || at < since {
            continue;
        }
        let mut lines = row.lines;
        let weight = |l: &TillLine| l.total_cents.unwrap_or(0).abs() as i128;
        let by_total = lines.iter().any(|l| weight(l) > 0);
        let weights: Vec<i128> = lines
            .iter()
            .map(|l| {
                if by_total {
                    weight(l)
                } else {
                    (l.quantity.abs() * 1000.0) as i128
                }
            })
            .collect();
        let sum: i128 = weights.iter().sum();
        if lines.is_empty() || sum == 0 {
            lines = vec![TillLine {
                description: "Till sale".into(),
                quantity: 1.0,
                total_cents: None,
            }];
        }
        let n = lines.len();
        let mut left = row.total_cents;
        for (i, line) in lines.into_iter().enumerate() {
            let share = if i + 1 == n || sum == 0 {
                left
            } else {
                (row.total_cents as i128 * weights[i] / sum) as i64
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
                unit_price: CurrencyAmount::new(share as f64 / 100.0, row.currency.as_str()),
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
    }
    out
}

/// Friday's stock: tracked products at or under their reorder point.
pub async fn low_stock(friday: &FridayClient, token: &str) -> Result<Vec<StockLevel>, FridayError> {
    let rows = friday.get(token, "/api/inventory/products").await?;
    let products: Vec<Product> = serde_json::from_value(rows).map_err(|e| FridayError::Status {
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
}
