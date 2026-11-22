//! Scans from Friday's /telegram-scan.html (plan 16.3). The page sends a
//! batch, `{"m": "sell"|"stock"|"check", "c": [["<code>", 2], ...]}` (a bare
//! code from an older page counts as one Check). Codes are products in
//! Friday's Inventory, found by barcode then SKU, as the person scanning
//! sees them: Sell records them as sales at Inventory's price, Stock in as
//! stock received, Check just shows them.

use chrono::Utc;
use guss_core::domain::{BusinessEvent, BusinessProfile, Sale, SourceChannel, StockUpdate};
use guss_core::money::CurrencyAmount;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Sell,
    Stock,
    Check,
    /// Friday Fleets parcels: "in", "out", "delivered" or "returned".
    Parcel(&'static str),
}

/// What the page sent: the mode and each code with how many times it was
/// scanned.
pub fn parse(data: &str) -> (Mode, Vec<(String, f64)>) {
    // An object is a batch; anything else (a bare code, which can itself
    // parse as a JSON number) is one code.
    let Some(v) = serde_json::from_str::<Value>(data)
        .ok()
        .filter(Value::is_object)
    else {
        return (Mode::Check, vec![(data.trim().to_string(), 1.0)]);
    };
    let mode = match v.get("m").and_then(Value::as_str) {
        Some("sell") => Mode::Sell,
        Some("stock") => Mode::Stock,
        Some("parcel") => Mode::Parcel(match v.get("a").and_then(Value::as_str) {
            Some("out") => "out",
            Some("delivered") => "delivered",
            Some("returned") => "returned",
            _ => "in",
        }),
        _ => Mode::Check,
    };
    let codes = v
        .get("c")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|pair| {
            let code = pair.get(0)?.as_str()?.trim().to_string();
            let n = pair.get(1).and_then(Value::as_f64).unwrap_or(1.0);
            (!code.is_empty() && n > 0.0 && n <= 10_000.0).then_some((code, n))
        })
        .take(200)
        .collect();
    (mode, codes)
}

/// The product with this barcode or SKU.
pub fn matching<'a>(products: &'a Value, code: &str) -> Option<&'a Value> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    let field = |p: &'a Value, k: &str| p.get(k).and_then(Value::as_str).map(str::trim);
    let list = products.as_array()?;
    list.iter()
        .find(|p| field(p, "barcode") == Some(code))
        .or_else(|| {
            list.iter().find(|p| {
                field(p, "sku").is_some_and(|s| !s.is_empty() && s.eq_ignore_ascii_case(code))
            })
        })
}

fn name(p: &Value) -> String {
    p.get("name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

fn price_cents(p: &Value) -> i64 {
    p.get("price_cents").and_then(Value::as_i64).unwrap_or(0)
}

/// "Sugar 1kg · 150.00 KES · 23 in stock".
pub fn product_line(p: &Value, currency: &str) -> String {
    let price = price_cents(p) as f64 / 100.0;
    let stock = p.get("on_hand_qty").and_then(|q| {
        q.as_f64()
            .or_else(|| q.as_str().and_then(|s| s.parse().ok()))
    });
    let mut line = format!("{} · {price:.2} {currency}", name(p));
    if let Some(q) = stock {
        line.push_str(&format!(" · {q} in stock"));
    }
    line
}

/// The entries a Sell or Stock-in batch makes, plus a line for each code
/// that can't be recorded (unknown, or no price to sell at).
pub fn events(
    mode: Mode,
    products: &Value,
    codes: &[(String, f64)],
    business: &BusinessProfile,
) -> (Vec<BusinessEvent>, Vec<String>) {
    let currency = business.default_currency.trim().to_uppercase();
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for (code, n) in codes {
        let Some(p) = matching(products, code) else {
            problems.push(format!(
                "No product in Friday has the code {code} (Inventory, barcode field)."
            ));
            continue;
        };
        let raw_text = Some(format!("scanned {code} x{n}"));
        match mode {
            Mode::Sell if price_cents(p) <= 0 => problems.push(format!(
                "{} has no price in Friday; say \"sold {n} {} at <price>\".",
                name(p),
                name(p)
            )),
            Mode::Sell => out.push(BusinessEvent::Sale(Sale {
                id: Uuid::new_v4(),
                business_id: business.id,
                item_name: name(p),
                quantity: *n,
                unit: None,
                unit_factor: 1.0,
                unit_price: CurrencyAmount::new(price_cents(p) as f64 / 100.0, currency.as_str()),
                category: None,
                occurred_at: Utc::now(),
                recurring: None,
                source_channel: SourceChannel::Telegram,
                paid_by: None,
                credit_to: None,
                returns: None,
                raw_text,
            })),
            // No cost: a count of what came in, not a purchase (that's a PO).
            Mode::Stock => out.push(BusinessEvent::StockUpdate(StockUpdate {
                id: Uuid::new_v4(),
                business_id: business.id,
                item_name: name(p),
                delta: *n,
                unit: None,
                unit_factor: 1.0,
                unit_cost: None,
                reorder_threshold: None,
                occurred_at: Utc::now(),
                source_channel: SourceChannel::Telegram,
                raw_text,
            })),
            Mode::Check | Mode::Parcel(_) => {}
        }
    }
    (out, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shop() -> BusinessProfile {
        BusinessProfile {
            id: Uuid::new_v4(),
            name: "Duka".into(),
            default_currency: "KES".into(),
            language: "en".into(),
            timezone: "UTC".into(),
        }
    }

    fn products() -> Value {
        json!([
            {"name": "Sugar 1kg", "barcode": "6161100", "sku": "SUG1", "price_cents": 15000, "on_hand_qty": "23.000"},
            {"name": "Rice", "barcode": "", "sku": "6161100X", "price_cents": 12000, "on_hand_qty": 4},
            {"name": "Sample", "barcode": "999", "sku": "", "price_cents": 0}
        ])
    }

    #[test]
    fn barcode_first_then_sku() {
        let p = products();
        assert_eq!(
            product_line(matching(&p, " 6161100 ").unwrap(), "KES"),
            "Sugar 1kg · 150.00 KES · 23 in stock"
        );
        assert_eq!(matching(&p, "6161100x").unwrap()["name"], "Rice");
        assert!(matching(&p, "").is_none());
    }

    #[test]
    fn a_batch_or_an_old_bare_code() {
        assert_eq!(
            parse(r#"{"m":"sell","c":[["6161100",2],["X",1],["",3],["Y",-1]]}"#),
            (Mode::Sell, vec![("6161100".into(), 2.0), ("X".into(), 1.0)])
        );
        assert_eq!(
            parse(" 6161100 "),
            (Mode::Check, vec![("6161100".into(), 1.0)])
        );
        assert_eq!(
            parse(r#"{"m":"parcel","a":"delivered","c":[["WB-1",1]]}"#),
            (Mode::Parcel("delivered"), vec![("WB-1".into(), 1.0)])
        );
        assert_eq!(
            parse(r#"{"m":"parcel","c":[["WB-1",1]]}"#).0,
            Mode::Parcel("in")
        );
    }

    #[test]
    fn sell_and_stock_batches_make_entries() {
        let codes = vec![
            ("6161100".into(), 2.0),
            ("nope".into(), 1.0),
            ("999".into(), 1.0),
        ];
        let (sold, problems) = events(Mode::Sell, &products(), &codes, &shop());
        assert_eq!(sold.len(), 1);
        match &sold[0] {
            BusinessEvent::Sale(s) => {
                assert_eq!((s.item_name.as_str(), s.quantity), ("Sugar 1kg", 2.0));
                assert_eq!(s.total().to_string(), "300.00 KES");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[1].starts_with("Sample has no price"));

        let (came_in, _) = events(Mode::Stock, &products(), &codes[..1], &shop());
        assert!(
            matches!(&came_in[0], BusinessEvent::StockUpdate(su) if su.delta == 2.0 && su.unit_cost.is_none())
        );
    }
}
