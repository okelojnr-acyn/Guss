//! Extraction eval: real shop messages (`evals/cases.jsonl`) against a
//! real model, scored per category. Measures tuning instead of guessing:
//! run it before and after any prompt or extraction change.
//!
//! ```sh
//! GUSS_EVAL_BASE_URL=https://api.groq.com/openai/v1 \
//! GUSS_EVAL_API_KEY=... GUSS_EVAL_MODEL=openai/gpt-oss-120b \
//! cargo test -p guss-nlu --test eval_live -- --ignored --nocapture
//! ```
//!
//! `GUSS_EVAL_ONLY=typos,arithmetic` limits it to some categories.
//! The scoring itself is covered by the non-ignored tests below.

use std::collections::BTreeMap;

use guss_core::domain::{BusinessEvent, SourceChannel};
use guss_llm::OpenAiCompatibleProvider;
use guss_nlu::Nlu;
use serde::Deserialize;
use uuid::Uuid;

const CASES: &str = include_str!("../evals/cases.jsonl");

#[derive(Debug, Clone, Deserialize)]
struct Case {
    id: String,
    category: String,
    lang: String,
    #[serde(default = "default_currency")]
    currency: String,
    text: String,
    /// "nothing" (store nothing at all) or "ask" (no money entries: the
    /// message is unclear or wrong and must not be guessed).
    #[serde(default)]
    expect: Option<String>,
    #[serde(default)]
    events: Option<Vec<Want>>,
    /// Any one of these event sets is acceptable.
    #[serde(default)]
    any_of: Option<Vec<WantSet>>,
    #[serde(default)]
    money_in: Option<f64>,
    #[serde(default)]
    money_out: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct WantSet {
    events: Vec<Want>,
}

#[derive(Debug, Clone, Deserialize)]
struct Want {
    kind: String,
    item: Option<String>,
    qty: Option<f64>,
    unit_price: Option<f64>,
    amount: Option<f64>,
    delta: Option<f64>,
    unit_cost: Option<f64>,
}

fn default_currency() -> String {
    "KES".into()
}

fn cases() -> Vec<Case> {
    CASES
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("cases.jsonl line {}: {e}", i + 1))
        })
        .collect()
}

/// "T-Shirts" == "tshirt", "eggs" == "egg"; containment so "coke 500ml"
/// matches "coke".
fn same_item(want: &str, got: &str) -> bool {
    let n = |s: &str| {
        let s: String = s
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        s.strip_suffix('s').map(str::to_string).unwrap_or(s)
    };
    let (w, g) = (n(want), n(got));
    !w.is_empty() && (g.contains(&w) || w.contains(&g) && !g.is_empty())
}

fn close(want: Option<f64>, got: f64) -> bool {
    want.is_none_or(|w| (w - got).abs() < 0.005)
}

fn matches(want: &Want, got: &BusinessEvent) -> bool {
    let item_ok = |name: &str| want.item.as_deref().is_none_or(|w| same_item(w, name));
    match (want.kind.as_str(), got) {
        ("sale", BusinessEvent::Sale(s)) => {
            item_ok(&s.item_name)
                && close(want.qty, s.quantity)
                && close(want.unit_price, s.unit_price.major_units())
        }
        ("expense", BusinessEvent::Expense(e)) => close(want.amount, e.amount.major_units()),
        ("stock", BusinessEvent::StockUpdate(su)) => {
            item_ok(&su.item_name)
                && close(want.delta, su.delta)
                && close(
                    want.unit_cost,
                    su.unit_cost.as_ref().map_or(f64::NAN, |c| c.major_units()),
                )
        }
        _ => false,
    }
}

fn is_money(e: &BusinessEvent) -> bool {
    !matches!(e, BusinessEvent::Note(_))
}

/// Money in from sales and out from expenses plus paid-for restocks,
/// computed the way Guss should count it (code, not the model).
fn money(events: &[BusinessEvent]) -> (f64, f64) {
    let mut inn = 0.0;
    let mut out = 0.0;
    for e in events {
        match e {
            BusinessEvent::Sale(s) => inn += s.total().major_units(),
            BusinessEvent::Expense(x) => out += x.amount.major_units(),
            BusinessEvent::StockUpdate(su) if su.delta > 0.0 => {
                if let Some(c) = &su.unit_cost {
                    out += su.delta * c.major_units();
                }
            }
            _ => {}
        }
    }
    (inn, out)
}

/// Every wanted event matches a distinct extracted one, and no money
/// entry is left over (extra notes are tolerated).
fn set_matches(wants: &[Want], got: &[BusinessEvent]) -> bool {
    let mut used = vec![false; got.len()];
    for w in wants {
        match (0..got.len()).find(|&i| !used[i] && matches(w, &got[i])) {
            Some(i) => used[i] = true,
            None => return false,
        }
    }
    got.iter().zip(&used).all(|(e, used)| *used || !is_money(e))
}

/// `None` = pass, `Some(reason)` = fail.
fn score(case: &Case, got: &[BusinessEvent]) -> Option<String> {
    match case.expect.as_deref() {
        Some("nothing") if !got.is_empty() => return Some("should store nothing".into()),
        Some("ask") if got.iter().any(is_money) => {
            return Some("guessed money entries instead of asking".into());
        }
        _ => {}
    }
    if let Some(wants) = &case.events
        && !set_matches(wants, got)
    {
        return Some("events differ".into());
    }
    if let Some(sets) = &case.any_of
        && !sets.iter().any(|s| set_matches(&s.events, got))
    {
        return Some("matches none of the accepted readings".into());
    }
    let (inn, out) = money(got);
    if !close(case.money_in, inn) {
        return Some(format!("money in {inn}, want {}", case.money_in.unwrap()));
    }
    if !close(case.money_out, out) {
        return Some(format!("money out {out}, want {}", case.money_out.unwrap()));
    }
    None
}

fn describe(events: &[BusinessEvent]) -> String {
    let parts: Vec<String> = events
        .iter()
        .map(|e| match e {
            BusinessEvent::Sale(s) => format!(
                "sale {} x{} @{}",
                s.item_name,
                s.quantity,
                s.unit_price.major_units()
            ),
            BusinessEvent::Expense(x) => {
                format!("expense '{}' {}", x.description, x.amount.major_units())
            }
            BusinessEvent::StockUpdate(su) => format!(
                "stock {} {:+}{}",
                su.item_name,
                su.delta,
                su.unit_cost
                    .as_ref()
                    .map(|c| format!(" @{}", c.major_units()))
                    .unwrap_or_default()
            ),
            BusinessEvent::Note(n) => format!("note '{}'", n.text),
            BusinessEvent::DebtPayment(d) => {
                format!("paid debt {} {}", d.customer, d.amount.major_units())
            }
        })
        .collect();
    if parts.is_empty() {
        "(nothing)".into()
    } else {
        parts.join("; ")
    }
}

#[tokio::test]
#[ignore = "calls a real LLM; set GUSS_EVAL_BASE_URL / GUSS_EVAL_API_KEY / GUSS_EVAL_MODEL"]
async fn eval_live() {
    let base = std::env::var("GUSS_EVAL_BASE_URL").expect("GUSS_EVAL_BASE_URL");
    let key = std::env::var("GUSS_EVAL_API_KEY").ok();
    let model = std::env::var("GUSS_EVAL_MODEL").expect("GUSS_EVAL_MODEL");
    let only: Option<Vec<String>> = std::env::var("GUSS_EVAL_ONLY")
        .ok()
        .map(|s| s.split(',').map(|c| c.trim().to_string()).collect());

    let mut per_cat: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut failures = Vec::new();
    for case in cases() {
        if only.as_ref().is_some_and(|o| !o.contains(&case.category)) {
            continue;
        }
        let nlu = Nlu::new(
            OpenAiCompatibleProvider::new(base.clone(), key.clone(), model.clone()),
            case.lang.clone(),
        );
        let result = nlu
            .extract(
                Uuid::new_v4(),
                SourceChannel::Cli,
                &case.text,
                &case.currency,
                &[],
            )
            .await;
        let entry = per_cat.entry(case.category.clone()).or_default();
        entry.1 += 1;
        let verdict = match &result {
            Ok(events) => score(&case, events).map(|why| (why, describe(events))),
            Err(e) => Some(("extraction error".into(), e.to_string())),
        };
        match verdict {
            None => entry.0 += 1,
            Some((why, got)) => failures.push(format!(
                "  {:<28} {why}\n      said: {}\n      got:  {got}",
                case.id, case.text
            )),
        }
    }

    let (pass, total) = per_cat
        .values()
        .fold((0, 0), |(p, t), (cp, ct)| (p + cp, t + ct));
    println!("\nGuss extraction eval - model {model}");
    for (cat, (p, t)) in &per_cat {
        println!("  {cat:<14} {p:>3}/{t:<3}");
    }
    println!("  {:<14} {pass:>3}/{total:<3}\n", "TOTAL");
    if !failures.is_empty() {
        println!("Failures:\n{}", failures.join("\n"));
    }
}

#[cfg(test)]
mod scoring {
    use super::*;
    use chrono::Utc;
    use guss_core::domain::{Expense, Note, Sale, StockUpdate};
    use guss_core::money::CurrencyAmount;

    fn sale(item: &str, qty: f64, price: f64) -> BusinessEvent {
        BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id: Uuid::nil(),
            item_name: item.into(),
            quantity: qty,
            unit: None,
            unit_factor: 1.0,
            unit_price: CurrencyAmount::new(price, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            paid_by: None,
            credit_to: None,
            returns: None,
            raw_text: None,
        })
    }

    fn expense(amount: f64) -> BusinessEvent {
        BusinessEvent::Expense(Expense {
            id: Uuid::new_v4(),
            business_id: Uuid::nil(),
            description: "x".into(),
            amount: CurrencyAmount::new(amount, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            raw_text: None,
            reimburse: None,
        })
    }

    fn stock(item: &str, delta: f64, cost: Option<f64>) -> BusinessEvent {
        BusinessEvent::StockUpdate(StockUpdate {
            id: Uuid::new_v4(),
            business_id: Uuid::nil(),
            item_name: item.into(),
            delta,
            unit: None,
            unit_factor: 1.0,
            unit_cost: cost.map(|c| CurrencyAmount::new(c, "KES")),
            reorder_threshold: None,
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Cli,
            raw_text: None,
        })
    }

    fn note() -> BusinessEvent {
        BusinessEvent::Note(Note {
            id: Uuid::new_v4(),
            business_id: Uuid::nil(),
            text: "x".into(),
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Cli,
            paystack_ref: None,
        })
    }

    fn case(id: &str) -> Case {
        cases().into_iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn every_case_parses_and_ids_are_unique() {
        let all = cases();
        assert!(all.len() >= 40);
        let mut ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
    }

    #[test]
    fn owners_transcript_scores_as_it_happened() {
        // What Guss actually did for "nought 30 tshirts 650 each, ...".
        let got = vec![note(), expense(3124.0), expense(500.0)];
        assert!(score(&case("owner-nought-typo"), &got).is_some());

        let right = vec![
            stock("Tshirts", 30.0, Some(650.0)),
            expense(3124.0),
            expense(500.0),
        ];
        assert_eq!(score(&case("owner-nought-typo"), &right), None);
    }

    #[test]
    fn restock_without_cost_misses_money_out() {
        let got = vec![stock("tshirt", 30.0, None), expense(3124.0), expense(500.0)];
        let why = score(&case("owner-bought"), &got).unwrap();
        assert!(why.contains("events differ"), "{why}");
    }

    #[test]
    fn nothing_means_not_even_a_note() {
        assert!(score(&case("conv-thanks"), &[note()]).is_some());
        assert_eq!(score(&case("conv-thanks"), &[]), None);
    }

    #[test]
    fn ask_tolerates_notes_but_not_guesses() {
        assert_eq!(score(&case("odd-negative"), &[note()]), None);
        assert!(score(&case("odd-negative"), &[sale("rice", 3.0, -120.0)]).is_some());
    }

    #[test]
    fn item_names_match_loosely() {
        assert!(same_item("tshirt", "T-Shirts"));
        assert!(same_item("coke", "Coke 500ml"));
        assert!(same_item("egg", "eggs"));
        assert!(!same_item("rice", "bread"));
    }

    #[test]
    fn extra_money_entries_fail() {
        let got = vec![sale("rice", 3.0, 120.0), expense(10.0)];
        assert!(score(&case("math-unit-price"), &got).is_some());
        assert_eq!(score(&case("math-unit-price"), &got[..1]), None);
    }
}
