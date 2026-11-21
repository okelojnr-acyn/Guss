use std::cmp::Ordering;
use std::collections::HashMap;

use guss_core::domain::BusinessEvent;
use guss_core::money::{CurrencyAmount, Money};
use guss_core::store::{StockLevel, Store, StoreError, TimeWindow};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct ItemStat {
    pub item_name: String,
    pub quantity: f64,
    /// Revenue per currency the item was sold in, business currency first.
    /// Never summed across currencies.
    pub revenue: Vec<CurrencyAmount>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MrrLine {
    pub label: String,
    /// Monthly-normalized amount. This is a simple projection (daily*30,
    /// weekly*4.33, monthly as-is) summed across matching events in the
    /// window, NOT real subscription-cohort MRR - see docs/ARCHITECTURE.md.
    /// In the currency the events were recorded in.
    pub monthly_amount: CurrencyAmount,
}

/// Revenue, expenses and net for one currency. Amounts in different
/// currencies are never added together: there is no exchange rate here.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrencyTotals {
    pub currency: String,
    pub revenue: CurrencyAmount,
    pub expenses: CurrencyAmount,
    /// Paid for stock bought to resell (restocks with a cost).
    pub stock_bought: CurrencyAmount,
    /// Cash basis: revenue minus expenses minus stock bought.
    pub net_cash_basis: CurrencyAmount,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportData {
    pub window: TimeWindow,
    /// The business currency (upper-case ISO code).
    pub currency: String,
    /// Totals in the business currency (always present, zero if nothing
    /// was recorded in it).
    pub totals: CurrencyTotals,
    /// Totals for every other currency recorded in the window, sorted by
    /// currency code. Reported separately, never converted.
    pub other_currency_totals: Vec<CurrencyTotals>,
    /// Cost-aware margin in the business currency, only present when at
    /// least one item sold in the business currency also had a restock
    /// with a business-currency `unit_cost` in the window. Best-effort, not
    /// real COGS accounting - see docs/ARCHITECTURE.md.
    pub gross_margin: Option<CurrencyAmount>,
    pub top_sellers_by_quantity: Vec<ItemStat>,
    pub bottom_sellers_by_quantity: Vec<ItemStat>,
    /// Top 3 per currency (business currency first); each entry's
    /// `revenue` holds only the currency it was ranked in.
    pub top_sellers_by_revenue: Vec<ItemStat>,
    pub low_stock: Vec<StockLevel>,
    /// Items with a negative net stock movement in the window that never
    /// had a reorder threshold set - surfaced separately so they aren't
    /// silently invisible just because no threshold exists.
    pub declining_no_threshold: Vec<String>,
    pub recurring_revenue: Vec<MrrLine>,
    pub recurring_expenses: Vec<MrrLine>,
}

/// `timezone` is the shop's (e.g. "Africa/Nairobi"): "today" is its
/// calendar day, not the last 24 hours.
pub async fn build_report<S: Store + ?Sized>(
    store: &S,
    business_id: Uuid,
    window: TimeWindow,
    currency: &str,
    timezone: &str,
) -> Result<ReportData, StoreError> {
    let since = window.start_in(chrono::Utc::now(), timezone);
    let events = store.list_events_since(business_id, since).await?;
    let low_stock = store.low_stock_items(business_id).await?;
    Ok(compute_report(&events, low_stock, window, currency))
}

fn norm(currency: &str) -> String {
    currency.trim().to_uppercase()
}

fn amount(minor_units: i64, currency: &str) -> CurrencyAmount {
    CurrencyAmount {
        amount: Money::from_minor_units(minor_units),
        currency: currency.to_string(),
    }
}

/// Business currency first, then the rest alphabetically.
fn currency_cmp(home: &str, a: &str, b: &str) -> Ordering {
    (a != home, a).cmp(&(b != home, b))
}

pub fn compute_report(
    events: &[BusinessEvent],
    low_stock: Vec<StockLevel>,
    window: TimeWindow,
    currency: &str,
) -> ReportData {
    let home = norm(currency);
    // item -> quantity sold (units are units, whatever the price currency)
    let mut qty_by_item: HashMap<String, f64> = HashMap::new();
    // (item, currency) -> (quantity, revenue_minor)
    let mut sales_by_item_ccy: HashMap<(String, String), (f64, i64)> = HashMap::new();
    let mut stock_delta_by_item: HashMap<String, f64> = HashMap::new();
    let mut threshold_ever_set: HashMap<String, bool> = HashMap::new();
    // item -> (qty*cost_sum, qty_sum) for positive deltas with a business-currency cost
    let mut restock_cost_weighted: HashMap<String, (f64, f64)> = HashMap::new();
    // currency -> (revenue_minor, expenses_minor, stock_bought_minor)
    let mut totals: HashMap<String, (i64, i64, i64)> = HashMap::new();
    // (label, currency) -> monthly major amount
    let mut recurring_revenue: HashMap<(String, String), f64> = HashMap::new();
    let mut recurring_expenses: HashMap<(String, String), f64> = HashMap::new();

    for event in events {
        match event {
            BusinessEvent::Sale(sale) => {
                let total = sale.total();
                let ccy = norm(&total.currency);
                let revenue_minor = total.amount.minor_units;
                totals.entry(ccy.clone()).or_default().0 += revenue_minor;

                // Quantities in the item's base unit (2 crates of 24 = 48).
                *qty_by_item.entry(sale.item_name.clone()).or_insert(0.0) += sale.base_quantity();
                let entry = sales_by_item_ccy
                    .entry((sale.item_name.clone(), ccy.clone()))
                    .or_insert((0.0, 0));
                entry.0 += sale.base_quantity();
                entry.1 += revenue_minor;

                *stock_delta_by_item
                    .entry(sale.item_name.clone())
                    .or_insert(0.0) -= sale.base_quantity();

                if let Some(r) = &sale.recurring {
                    let major = amount(revenue_minor, &ccy).major_units();
                    *recurring_revenue
                        .entry((r.label.clone(), ccy))
                        .or_insert(0.0) += major * r.interval.monthly_factor();
                }
            }
            BusinessEvent::Expense(expense) => {
                let ccy = norm(&expense.amount.currency);
                let minor = expense.amount.amount.minor_units;
                totals.entry(ccy.clone()).or_default().1 += minor;
                if let Some(r) = &expense.recurring {
                    let major = amount(minor, &ccy).major_units();
                    *recurring_expenses
                        .entry((r.label.clone(), ccy))
                        .or_insert(0.0) += major * r.interval.monthly_factor();
                }
            }
            BusinessEvent::StockUpdate(su) => {
                *stock_delta_by_item
                    .entry(su.item_name.clone())
                    .or_insert(0.0) += su.base_delta();
                if su.reorder_threshold.is_some() {
                    threshold_ever_set.insert(su.item_name.clone(), true);
                }
                if su.delta > 0.0
                    && let Some(cost) = &su.unit_cost
                {
                    let ccy = norm(&cost.currency);
                    totals.entry(ccy).or_default().2 +=
                        (su.delta * cost.amount.minor_units as f64).round() as i64;
                }
                if su.delta > 0.0
                    && let Some(cost) = &su.unit_cost
                    && norm(&cost.currency) == home
                {
                    let entry = restock_cost_weighted
                        .entry(su.item_name.clone())
                        .or_insert((0.0, 0.0));
                    // Cost per base unit, weighted by base units bought.
                    entry.0 += su.delta * cost.major_units();
                    entry.1 += su.base_delta();
                }
            }
            BusinessEvent::Note(_) => {}
        }
    }

    let item_revenue = |item: &str| -> Vec<CurrencyAmount> {
        let mut v: Vec<(&String, i64)> = sales_by_item_ccy
            .iter()
            .filter(|((name, _), _)| name == item)
            .map(|((_, ccy), (_, rev))| (ccy, *rev))
            .collect();
        v.sort_by(|a, b| currency_cmp(&home, a.0, b.0));
        v.into_iter().map(|(ccy, rev)| amount(rev, ccy)).collect()
    };

    let mut by_quantity: Vec<ItemStat> = qty_by_item
        .iter()
        .map(|(name, qty)| ItemStat {
            item_name: name.clone(),
            quantity: *qty,
            revenue: item_revenue(name),
        })
        .collect();
    // Ties broken by name so the report reads the same run to run.
    by_quantity.sort_by(|a, b| {
        b.quantity
            .partial_cmp(&a.quantity)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.item_name.cmp(&b.item_name))
    });

    let top_sellers_by_quantity = by_quantity.iter().take(3).cloned().collect();
    let bottom_sellers_by_quantity = by_quantity.iter().rev().take(3).cloned().collect();

    let mut currencies: Vec<String> = totals.keys().cloned().collect();
    currencies.sort_by(|a, b| currency_cmp(&home, a, b));

    let mut top_sellers_by_revenue = Vec::new();
    for ccy in &currencies {
        let mut in_ccy: Vec<(&String, f64, i64)> = sales_by_item_ccy
            .iter()
            .filter(|((_, c), _)| c == ccy)
            .map(|((name, _), (qty, rev))| (name, *qty, *rev))
            .collect();
        in_ccy.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
        top_sellers_by_revenue.extend(in_ccy.into_iter().take(3).map(|(name, qty, rev)| {
            ItemStat {
                item_name: name.clone(),
                quantity: qty,
                revenue: vec![amount(rev, ccy)],
            }
        }));
    }

    let mut declining_no_threshold: Vec<String> = stock_delta_by_item
        .iter()
        .filter(|(name, delta)| **delta < 0.0 && !threshold_ever_set.contains_key(*name))
        .map(|(name, _)| name.clone())
        .collect();
    declining_no_threshold.sort();

    let make_totals = |ccy: &str| -> CurrencyTotals {
        let (rev, exp, stock) = totals.get(ccy).copied().unwrap_or((0, 0, 0));
        CurrencyTotals {
            currency: ccy.to_string(),
            revenue: amount(rev, ccy),
            expenses: amount(exp, ccy),
            stock_bought: amount(stock, ccy),
            net_cash_basis: amount(rev - exp - stock, ccy),
        }
    };
    let home_totals = make_totals(&home);
    let other_currency_totals: Vec<CurrencyTotals> = currencies
        .iter()
        .filter(|c| **c != home)
        .map(|c| make_totals(c))
        .collect();

    // Gross margin, business currency only: items sold in the business
    // currency that also have a business-currency restock cost in this
    // window. Items without cost data are skipped rather than guessed.
    let mut cogs_minor: i64 = 0;
    let mut any_cost_known = false;
    for (item, (qty_cost_sum, qty_sum)) in &restock_cost_weighted {
        if *qty_sum <= 0.0 {
            continue;
        }
        let avg_cost_major = qty_cost_sum / qty_sum;
        if let Some((sold_qty, _)) = sales_by_item_ccy.get(&(item.clone(), home.clone())) {
            cogs_minor += CurrencyAmount::new(sold_qty * avg_cost_major, home.as_str())
                .amount
                .minor_units;
            any_cost_known = true;
        }
    }
    let gross_margin =
        any_cost_known.then(|| amount(home_totals.revenue.amount.minor_units - cogs_minor, &home));

    let mrr_lines = |m: HashMap<(String, String), f64>| -> Vec<MrrLine> {
        let mut v: Vec<MrrLine> = m
            .into_iter()
            .map(|((label, ccy), major)| MrrLine {
                label,
                monthly_amount: CurrencyAmount::new(major, ccy),
            })
            .collect();
        v.sort_by(|a, b| {
            currency_cmp(
                &home,
                &a.monthly_amount.currency,
                &b.monthly_amount.currency,
            )
            .then_with(|| a.label.cmp(&b.label))
        });
        v
    };
    let recurring_revenue = mrr_lines(recurring_revenue);
    let recurring_expenses = mrr_lines(recurring_expenses);

    ReportData {
        window,
        currency: home,
        totals: home_totals,
        other_currency_totals,
        gross_margin,
        top_sellers_by_quantity,
        bottom_sellers_by_quantity,
        top_sellers_by_revenue,
        low_stock,
        declining_no_threshold,
        recurring_revenue,
        recurring_expenses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use guss_core::domain::{
        Expense, RecurrenceInfo, RecurrenceInterval, Sale, SourceChannel, StockUpdate,
    };

    fn sale_in(item: &str, qty: f64, price: f64, currency: &str) -> BusinessEvent {
        BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id: Uuid::new_v4(),
            item_name: item.into(),
            quantity: qty,
            unit: None,
            unit_factor: 1.0,
            unit_price: CurrencyAmount::new(price, currency),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            paid_by: None,
            raw_text: None,
        })
    }

    fn sale(item: &str, qty: f64, price: f64) -> BusinessEvent {
        sale_in(item, qty, price, "USD")
    }

    fn expense_in(amount_major: f64, currency: &str) -> BusinessEvent {
        BusinessEvent::Expense(Expense {
            id: Uuid::new_v4(),
            business_id: Uuid::new_v4(),
            description: "rent".into(),
            amount: CurrencyAmount::new(amount_major, currency),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            raw_text: None,
        })
    }

    fn restock_in(
        item: &str,
        delta: f64,
        threshold: Option<f64>,
        unit_cost: Option<f64>,
        currency: &str,
    ) -> BusinessEvent {
        BusinessEvent::StockUpdate(StockUpdate {
            id: Uuid::new_v4(),
            business_id: Uuid::new_v4(),
            item_name: item.into(),
            delta,
            unit: None,
            unit_factor: 1.0,
            unit_cost: unit_cost.map(|c| CurrencyAmount::new(c, currency)),
            reorder_threshold: threshold,
            occurred_at: Utc::now(),
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
        restock_in(item, delta, threshold, unit_cost, "USD")
    }

    #[test]
    fn totals_and_best_seller_are_correct() {
        let events = vec![sale("rice", 3.0, 500.0), sale("soap", 1.0, 1200.0)];
        let report = compute_report(&events, vec![], TimeWindow::Day, "USD");

        assert_eq!(report.totals.revenue.amount.minor_units, 3 * 50000 + 120000);
        assert!(report.other_currency_totals.is_empty());
        assert_eq!(report.top_sellers_by_quantity[0].item_name, "rice");
    }

    #[test]
    fn currencies_are_never_added_together() {
        // The bug this guards: a $50 sale used to be counted as 50.00 KES.
        let events = vec![
            sale_in("rice", 3.0, 120.0, "KES"),
            sale_in("rice", 1.0, 50.0, "USD"),
            expense_in(100.0, "KES"),
            expense_in(5.0, "usd"),
        ];
        let report = compute_report(&events, vec![], TimeWindow::Day, "KES");

        assert_eq!(report.totals.currency, "KES");
        assert_eq!(report.totals.revenue, CurrencyAmount::new(360.0, "KES"));
        assert_eq!(report.totals.expenses, CurrencyAmount::new(100.0, "KES"));
        assert_eq!(
            report.totals.net_cash_basis,
            CurrencyAmount::new(260.0, "KES")
        );

        assert_eq!(report.other_currency_totals.len(), 1);
        let usd = &report.other_currency_totals[0];
        assert_eq!(usd.currency, "USD");
        assert_eq!(usd.revenue, CurrencyAmount::new(50.0, "USD"));
        assert_eq!(usd.expenses, CurrencyAmount::new(5.0, "USD"));
        assert_eq!(usd.net_cash_basis, CurrencyAmount::new(45.0, "USD"));

        // Quantity is unit-based, so it does add up; revenue stays split.
        let rice = &report.top_sellers_by_quantity[0];
        assert_eq!(rice.quantity, 4.0);
        assert_eq!(
            rice.revenue,
            vec![
                CurrencyAmount::new(360.0, "KES"),
                CurrencyAmount::new(50.0, "USD")
            ]
        );
    }

    #[test]
    fn money_paid_for_stock_counts_against_net() {
        // The owner's real day: transport, a subscription, 30 tshirts at 650.
        let events = vec![
            expense_in(500.0, "KES"),
            expense_in(3124.0, "KES"),
            restock_in("tshirt", 30.0, None, Some(650.0), "KES"),
            restock_in("tshirt", -2.0, None, None, "KES"),
        ];
        let report = compute_report(&events, vec![], TimeWindow::Day, "KES");
        assert_eq!(report.totals.expenses, CurrencyAmount::new(3624.0, "KES"));
        assert_eq!(
            report.totals.stock_bought,
            CurrencyAmount::new(19500.0, "KES")
        );
        assert_eq!(
            report.totals.net_cash_basis,
            CurrencyAmount::new(-23124.0, "KES")
        );
    }

    #[test]
    fn best_sellers_by_revenue_are_ranked_per_currency() {
        let events = vec![
            sale_in("soap", 1.0, 1000.0, "KES"),
            sale_in("rice", 1.0, 200.0, "KES"),
            // 900 USD would sit between them if the numbers were compared raw.
            sale_in("phone", 1.0, 900.0, "USD"),
        ];
        let report = compute_report(&events, vec![], TimeWindow::Day, "KES");
        let names: Vec<(&str, &str)> = report
            .top_sellers_by_revenue
            .iter()
            .map(|i| (i.item_name.as_str(), i.revenue[0].currency.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("soap", "KES"), ("rice", "KES"), ("phone", "USD")]
        );
    }

    #[test]
    fn foreign_only_day_still_reports_zero_in_business_currency() {
        let events = vec![sale_in("rice", 1.0, 50.0, "USD")];
        let report = compute_report(&events, vec![], TimeWindow::Day, "kes");
        assert_eq!(report.currency, "KES");
        assert_eq!(report.totals.revenue, CurrencyAmount::new(0.0, "KES"));
        assert_eq!(report.other_currency_totals[0].currency, "USD");
    }

    #[test]
    fn gross_margin_ignores_costs_in_other_currencies() {
        let events = vec![
            restock_in("rice", 10.0, None, Some(1.0), "USD"),
            sale_in("rice", 2.0, 120.0, "KES"),
        ];
        let report = compute_report(&events, vec![], TimeWindow::Day, "KES");
        assert!(report.gross_margin.is_none());
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

    #[test]
    fn recurring_lines_keep_their_own_currency() {
        let mut s = match sale_in("hosting", 1.0, 10.0, "USD") {
            BusinessEvent::Sale(s) => s,
            _ => unreachable!(),
        };
        s.recurring = Some(RecurrenceInfo {
            interval: RecurrenceInterval::Monthly,
            label: "Hosting".into(),
        });
        let report = compute_report(&[BusinessEvent::Sale(s)], vec![], TimeWindow::Month, "KES");
        assert_eq!(
            report.recurring_revenue[0].monthly_amount,
            CurrencyAmount::new(10.0, "USD")
        );
    }
}
