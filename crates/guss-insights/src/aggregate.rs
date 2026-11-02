use std::collections::HashMap;

use guss_core::domain::BusinessEvent;
use guss_core::money::CurrencyAmount;
use guss_core::store::{StockLevel, Store, StoreError, TimeWindow};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct ItemStat {
    pub item_name: String,
    pub quantity: f64,
    pub revenue: CurrencyAmount,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MrrLine {
    pub label: String,
    /// Monthly-normalized amount. This is a simple projection (daily*30,
    /// weekly*4.33, monthly as-is) summed across matching events in the
    /// window, NOT real subscription-cohort MRR - see docs/ARCHITECTURE.md.
    pub monthly_amount: CurrencyAmount,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportData {
    pub window: TimeWindow,
    pub currency: String,
    pub total_revenue: CurrencyAmount,
    pub total_expenses: CurrencyAmount,
    /// Cash-basis revenue minus expenses - always computed.
    pub net_cash_basis: CurrencyAmount,
    /// Cost-aware margin, only present when at least one item sold in the
    /// window also had a restock with `unit_cost` recorded in the window.
    /// Best-effort, not real COGS accounting - see docs/ARCHITECTURE.md.
    pub gross_margin: Option<CurrencyAmount>,
    pub top_sellers_by_quantity: Vec<ItemStat>,
    pub bottom_sellers_by_quantity: Vec<ItemStat>,
    pub top_sellers_by_revenue: Vec<ItemStat>,
    pub low_stock: Vec<StockLevel>,
    /// Items with a negative net stock movement in the window that never
    /// had a reorder threshold set - surfaced separately so they aren't
    /// silently invisible just because no threshold exists.
    pub declining_no_threshold: Vec<String>,
    pub recurring_revenue: Vec<MrrLine>,
    pub recurring_expenses: Vec<MrrLine>,
}

pub async fn build_report<S: Store + ?Sized>(
    store: &S,
    business_id: Uuid,
    window: TimeWindow,
    currency: &str,
) -> Result<ReportData, StoreError> {
    let events = store.list_events(business_id, window).await?;
    let low_stock = store.low_stock_items(business_id).await?;
    Ok(compute_report(&events, low_stock, window, currency))
}

pub fn compute_report(
    events: &[BusinessEvent],
    low_stock: Vec<StockLevel>,
    window: TimeWindow,
    currency: &str,
) -> ReportData {
    let mut sales_by_item: HashMap<String, (f64, i64)> = HashMap::new(); // item -> (quantity, revenue_minor)
    let mut stock_delta_by_item: HashMap<String, f64> = HashMap::new();
    let mut threshold_ever_set: HashMap<String, bool> = HashMap::new();
    let mut restock_cost_weighted: HashMap<String, (f64, f64)> = HashMap::new(); // item -> (qty*cost_sum, qty_sum) for positive deltas with cost
    let mut total_revenue_minor: i64 = 0;
    let mut total_expenses_minor: i64 = 0;
    let mut recurring_revenue: HashMap<String, f64> = HashMap::new(); // label -> monthly major amount
    let mut recurring_expenses: HashMap<String, f64> = HashMap::new();

    for event in events {
        match event {
            BusinessEvent::Sale(sale) => {
                let revenue_minor = sale.total().amount.minor_units;
                total_revenue_minor += revenue_minor;

                let entry = sales_by_item
                    .entry(sale.item_name.clone())
                    .or_insert((0.0, 0));
                entry.0 += sale.quantity;
                entry.1 += revenue_minor;

                *stock_delta_by_item
                    .entry(sale.item_name.clone())
                    .or_insert(0.0) -= sale.quantity;

                if let Some(r) = &sale.recurring {
                    let major = minor_to_major(revenue_minor, &sale.unit_price.currency);
                    *recurring_revenue.entry(r.label.clone()).or_insert(0.0) +=
                        major * r.interval.monthly_factor();
                }
            }
            BusinessEvent::Expense(expense) => {
                total_expenses_minor += expense.amount.amount.minor_units;
                if let Some(r) = &expense.recurring {
                    let major =
                        minor_to_major(expense.amount.amount.minor_units, &expense.amount.currency);
                    *recurring_expenses.entry(r.label.clone()).or_insert(0.0) +=
                        major * r.interval.monthly_factor();
                }
            }
            BusinessEvent::StockUpdate(su) => {
                *stock_delta_by_item
                    .entry(su.item_name.clone())
                    .or_insert(0.0) += su.delta;
                if su.reorder_threshold.is_some() {
                    threshold_ever_set.insert(su.item_name.clone(), true);
                }
                if su.delta > 0.0
                    && let Some(cost) = &su.unit_cost
                {
                    let cost_major = minor_to_major(cost.amount.minor_units, &cost.currency);
                    let entry = restock_cost_weighted
                        .entry(su.item_name.clone())
                        .or_insert((0.0, 0.0));
                    entry.0 += su.delta * cost_major;
                    entry.1 += su.delta;
                }
            }
            BusinessEvent::Note(_) => {}
        }
    }

    let mut by_quantity: Vec<ItemStat> = sales_by_item
        .iter()
        .map(|(name, (qty, rev))| ItemStat {
            item_name: name.clone(),
            quantity: *qty,
            revenue: CurrencyAmount {
                amount: guss_core::money::Money::from_minor_units(*rev),
                currency: currency.to_string(),
            },
        })
        .collect();
    by_quantity.sort_by(|a, b| b.quantity.partial_cmp(&a.quantity).unwrap());

    let mut by_revenue = by_quantity.clone();
    by_revenue.sort_by(|a, b| {
        b.revenue
            .amount
            .minor_units
            .cmp(&a.revenue.amount.minor_units)
    });

    let top_sellers_by_quantity = by_quantity.iter().take(3).cloned().collect();
    let bottom_sellers_by_quantity = {
        let mut rev = by_quantity.clone();
        rev.reverse();
        rev.into_iter().take(3).collect()
    };
    let top_sellers_by_revenue = by_revenue.iter().take(3).cloned().collect();

    let declining_no_threshold: Vec<String> = stock_delta_by_item
        .iter()
        .filter(|(name, delta)| **delta < 0.0 && !threshold_ever_set.contains_key(*name))
        .map(|(name, _)| name.clone())
        .collect();

    // Gross margin: only for items where we have a weighted-average restock
    // cost in this window; skip items without cost data entirely rather
    // than guessing.
    let mut cogs_minor: i64 = 0;
    let mut any_cost_known = false;
    for (item, (qty_cost_sum, qty_sum)) in &restock_cost_weighted {
        if *qty_sum <= 0.0 {
            continue;
        }
        let avg_cost_major = qty_cost_sum / qty_sum;
        if let Some((sold_qty, _)) = sales_by_item.get(item) {
            cogs_minor += CurrencyAmount::new(sold_qty * avg_cost_major, currency.to_string())
                .amount
                .minor_units;
            any_cost_known = true;
        }
    }
    let gross_margin = if any_cost_known {
        Some(CurrencyAmount {
            amount: guss_core::money::Money::from_minor_units(total_revenue_minor - cogs_minor),
            currency: currency.to_string(),
        })
    } else {
        None
    };

    ReportData {
        window,
        currency: currency.to_string(),
        total_revenue: CurrencyAmount {
            amount: guss_core::money::Money::from_minor_units(total_revenue_minor),
            currency: currency.to_string(),
        },
        total_expenses: CurrencyAmount {
            amount: guss_core::money::Money::from_minor_units(total_expenses_minor),
            currency: currency.to_string(),
        },
        net_cash_basis: CurrencyAmount {
            amount: guss_core::money::Money::from_minor_units(
                total_revenue_minor - total_expenses_minor,
            ),
            currency: currency.to_string(),
        },
        gross_margin,
        top_sellers_by_quantity,
        bottom_sellers_by_quantity,
        top_sellers_by_revenue,
        low_stock,
        declining_no_threshold,
        recurring_revenue: recurring_revenue
            .into_iter()
            .map(|(label, major)| MrrLine {
                label,
                monthly_amount: CurrencyAmount::new(major, currency.to_string()),
            })
            .collect(),
        recurring_expenses: recurring_expenses
            .into_iter()
            .map(|(label, major)| MrrLine {
                label,
                monthly_amount: CurrencyAmount::new(major, currency.to_string()),
            })
            .collect(),
    }
}

fn minor_to_major(minor_units: i64, currency: &str) -> f64 {
    CurrencyAmount {
        amount: guss_core::money::Money::from_minor_units(minor_units),
        currency: currency.to_string(),
    }
    .amount
    .format_in(currency)
    .parse()
    .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use guss_core::domain::{RecurrenceInfo, RecurrenceInterval, Sale, SourceChannel, StockUpdate};

    fn sale(item: &str, qty: f64, price: f64) -> BusinessEvent {
        BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id: Uuid::new_v4(),
            item_name: item.into(),
            quantity: qty,
            unit_price: CurrencyAmount::new(price, "USD"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            raw_text: None,
        })
    }

    fn restock(
        item: &str,
        delta: f64,
        threshold: Option<f64>,
        unit_cost: Option<f64>,
    ) -> BusinessEvent {
        BusinessEvent::StockUpdate(StockUpdate {
            id: Uuid::new_v4(),
            business_id: Uuid::new_v4(),
            item_name: item.into(),
            delta,
            unit: None,
            unit_cost: unit_cost.map(|c| CurrencyAmount::new(c, "USD")),
            reorder_threshold: threshold,
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Cli,
            raw_text: None,
        })
    }

    #[test]
    fn totals_and_best_seller_are_correct() {
        let events = vec![sale("rice", 3.0, 500.0), sale("soap", 1.0, 1200.0)];
        let report = compute_report(&events, vec![], TimeWindow::Day, "USD");

        assert_eq!(report.total_revenue.amount.minor_units, 3 * 50000 + 120000);
        assert_eq!(report.top_sellers_by_quantity[0].item_name, "rice");
    }

    #[test]
    fn declining_no_threshold_flags_items_without_any_threshold() {
        let events = vec![restock("rice", 5.0, None, None), sale("rice", 8.0, 500.0)];
        let report = compute_report(&events, vec![], TimeWindow::Day, "USD");

        assert_eq!(report.declining_no_threshold, vec!["rice".to_string()]);
    }

    #[test]
    fn declining_item_with_threshold_is_not_in_no_threshold_bucket() {
        let events = vec![
            restock("rice", 5.0, Some(10.0), None),
            sale("rice", 8.0, 500.0),
        ];
        let report = compute_report(&events, vec![], TimeWindow::Day, "USD");

        assert!(report.declining_no_threshold.is_empty());
    }

    #[test]
    fn gross_margin_present_only_when_restock_cost_known() {
        let events = vec![sale("rice", 10.0, 5.0)];
        let report_no_cost = compute_report(&events, vec![], TimeWindow::Day, "USD");
        assert!(report_no_cost.gross_margin.is_none());

        let events_with_cost = vec![
            restock("rice", 20.0, None, Some(2.0)),
            sale("rice", 10.0, 5.0),
        ];
        let report_with_cost = compute_report(&events_with_cost, vec![], TimeWindow::Day, "USD");
        assert!(report_with_cost.gross_margin.is_some());
        // revenue 50.00 - cogs (10 * 2.00) = 30.00
        assert_eq!(
            report_with_cost.gross_margin.unwrap().amount.minor_units,
            3000
        );
    }

    #[test]
    fn recurring_sale_normalizes_to_monthly_projection() {
        let mut recurring_sale = match sale("retainer", 1.0, 100.0) {
            BusinessEvent::Sale(s) => s,
            _ => unreachable!(),
        };
        recurring_sale.recurring = Some(RecurrenceInfo {
            interval: RecurrenceInterval::Weekly,
            label: "Acme retainer".into(),
        });
        let report = compute_report(
            &[BusinessEvent::Sale(recurring_sale)],
            vec![],
            TimeWindow::Week,
            "USD",
        );

        assert_eq!(report.recurring_revenue.len(), 1);
        assert_eq!(report.recurring_revenue[0].label, "Acme retainer");
        // 100 * 4.33 = 433.00
        assert_eq!(
            report.recurring_revenue[0]
                .monthly_amount
                .amount
                .minor_units,
            43300
        );
    }
}
