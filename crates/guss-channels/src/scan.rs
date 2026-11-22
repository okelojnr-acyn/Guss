//! What a scanned code is (plan 16.3): a product in Friday's Inventory,
//! found by barcode or SKU, as the person who scanned it sees it.

use serde_json::Value;

use crate::friday::{FridayClient, FridayError};

/// The product with this barcode or SKU, if the person may see it.
pub async fn find_product(
    friday: &FridayClient,
    token: &str,
    code: &str,
) -> Result<Option<Value>, FridayError> {
    let products = friday.get(token, "/api/inventory/products").await?;
    Ok(matching(&products, code).cloned())
}

fn matching<'a>(products: &'a Value, code: &str) -> Option<&'a Value> {
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

/// "Sugar 1kg · 150.00 KES · 23 in stock".
pub fn product_line(p: &Value, currency: &str) -> String {
    let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
    let price = p.get("price_cents").and_then(Value::as_i64).unwrap_or(0) as f64 / 100.0;
    let stock = p.get("on_hand_qty").and_then(|q| {
        q.as_f64()
            .or_else(|| q.as_str().and_then(|s| s.parse().ok()))
    });
    let mut line = format!("{name} · {price:.2} {currency}");
    if let Some(q) = stock {
        line.push_str(&format!(" · {q} in stock"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn barcode_first_then_sku() {
        let products = json!([
            {"name": "Sugar 1kg", "barcode": "6161100", "sku": "SUG1", "price_cents": 15000, "on_hand_qty": "23.000"},
            {"name": "Rice", "barcode": "", "sku": "6161100X", "price_cents": 12000, "on_hand_qty": 4}
        ]);
        let p = matching(&products, " 6161100 ").unwrap();
        assert_eq!(
            product_line(p, "KES"),
            "Sugar 1kg · 150.00 KES · 23 in stock"
        );
        assert_eq!(matching(&products, "6161100x").unwrap()["name"], "Rice");
        assert!(matching(&products, "").is_none());
        assert!(matching(&products, "nope").is_none());
    }
}
