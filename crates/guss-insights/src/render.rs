use guss_core::store::TimeWindow;
use handlebars::Handlebars;
use serde::Serialize;

use crate::aggregate::ReportData;

const TEMPLATE_EN: &str = include_str!("../templates/en/report.hbs");
const TEMPLATE_ID: &str = include_str!("../templates/id/report.hbs");

#[derive(Serialize)]
struct ItemStatView {
    item_name: String,
    quantity: f64,
    revenue: String,
}

#[derive(Serialize)]
struct StockLevelView {
    item_name: String,
    current_quantity: f64,
    reorder_threshold: f64,
}

#[derive(Serialize)]
struct MrrLineView {
    label: String,
    monthly_amount: String,
}

#[derive(Serialize)]
struct RenderContext {
    business_name: String,
    window_label: String,
    total_revenue: String,
    total_expenses: String,
    net_cash_basis: String,
    gross_margin: Option<String>,
    top_sellers_by_quantity: Vec<ItemStatView>,
    bottom_sellers_by_quantity: Vec<ItemStatView>,
    top_sellers_by_revenue: Vec<ItemStatView>,
    low_stock: Vec<StockLevelView>,
    declining_no_threshold: Vec<String>,
    recurring_revenue: Vec<MrrLineView>,
    recurring_expenses: Vec<MrrLineView>,
}

fn window_label(window: TimeWindow, language: &str) -> String {
    match (window, language) {
        (TimeWindow::Day, "id") => "24 jam terakhir".to_string(),
        (TimeWindow::Week, "id") => "7 hari terakhir".to_string(),
        (TimeWindow::Month, "id") => "30 hari terakhir".to_string(),
        (TimeWindow::Day, _) => "the last 24 hours".to_string(),
        (TimeWindow::Week, _) => "the last 7 days".to_string(),
        (TimeWindow::Month, _) => "the last 30 days".to_string(),
    }
}

fn items_view(items: &[crate::aggregate::ItemStat]) -> Vec<ItemStatView> {
    items
        .iter()
        .map(|i| ItemStatView {
            item_name: i.item_name.clone(),
            quantity: i.quantity,
            revenue: i.revenue.to_string(),
        })
        .collect()
}

/// Renders a `ReportData` into a natural-language string in the given
/// language ("en" or "id"), ready to be handed to TTS and/or sent as text.
pub fn render_report(data: &ReportData, business_name: &str, language: &str) -> String {
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);

    let (template, lang_key) = match language {
        "id" => (TEMPLATE_ID, "id"),
        _ => (TEMPLATE_EN, "en"),
    };

    let context = RenderContext {
        business_name: business_name.to_string(),
        window_label: window_label(data.window, lang_key),
        total_revenue: data.total_revenue.to_string(),
        total_expenses: data.total_expenses.to_string(),
        net_cash_basis: data.net_cash_basis.to_string(),
        gross_margin: data.gross_margin.as_ref().map(|m| m.to_string()),
        top_sellers_by_quantity: items_view(&data.top_sellers_by_quantity),
        bottom_sellers_by_quantity: items_view(&data.bottom_sellers_by_quantity),
        top_sellers_by_revenue: items_view(&data.top_sellers_by_revenue),
        low_stock: data
            .low_stock
            .iter()
            .map(|s| StockLevelView {
                item_name: s.item_name.clone(),
                current_quantity: s.current_quantity,
                reorder_threshold: s.reorder_threshold.unwrap_or(0.0),
            })
            .collect(),
        declining_no_threshold: data.declining_no_threshold.clone(),
        recurring_revenue: data
            .recurring_revenue
            .iter()
            .map(|m| MrrLineView {
                label: m.label.clone(),
                monthly_amount: m.monthly_amount.to_string(),
            })
            .collect(),
        recurring_expenses: data
            .recurring_expenses
            .iter()
            .map(|m| MrrLineView {
                label: m.label.clone(),
                monthly_amount: m.monthly_amount.to_string(),
            })
            .collect(),
    };

    hb.render_template(template, &context)
        .unwrap_or_else(|e| format!("(report rendering failed: {e})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::{ItemStat, MrrLine, ReportData};
    use guss_core::money::CurrencyAmount;
    use guss_core::store::StockLevel;

    fn sample_report() -> ReportData {
        ReportData {
            window: TimeWindow::Day,
            currency: "USD".into(),
            total_revenue: CurrencyAmount::new(1500.0, "USD"),
            total_expenses: CurrencyAmount::new(200.0, "USD"),
            net_cash_basis: CurrencyAmount::new(1300.0, "USD"),
            gross_margin: Some(CurrencyAmount::new(900.0, "USD")),
            top_sellers_by_quantity: vec![ItemStat {
                item_name: "rice".into(),
                quantity: 10.0,
                revenue: CurrencyAmount::new(500.0, "USD"),
            }],
            bottom_sellers_by_quantity: vec![],
            top_sellers_by_revenue: vec![],
            low_stock: vec![StockLevel {
                item_name: "soap".into(),
                current_quantity: 2.0,
                reorder_threshold: Some(5.0),
            }],
            declining_no_threshold: vec!["candles".into()],
            recurring_revenue: vec![MrrLine {
                label: "Acme retainer".into(),
                monthly_amount: CurrencyAmount::new(433.0, "USD"),
            }],
            recurring_expenses: vec![],
        }
    }

    #[test]
    fn english_report_mentions_key_figures() {
        let text = render_report(&sample_report(), "Acme Shop", "en");
        assert!(text.contains("Acme Shop"));
        assert!(text.contains("1500.00 USD"));
        assert!(text.contains("rice"));
        assert!(text.contains("soap"));
        assert!(text.contains("candles"));
        assert!(text.contains("Acme retainer"));
    }

    #[test]
    fn indonesian_report_mentions_key_figures() {
        let text = render_report(&sample_report(), "Toko Acme", "id");
        assert!(text.contains("Toko Acme"));
        assert!(text.contains("1500.00 USD"));
        assert!(text.contains("rice"));
        assert!(text.contains("soap"));
    }
}
