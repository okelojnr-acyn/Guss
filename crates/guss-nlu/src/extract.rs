use chrono::Utc;
use guss_core::domain::{
    BusinessEvent, DebtPayment, Expense, Note, Sale, SourceChannel, StockUpdate,
};
use guss_core::money::CurrencyAmount;
use guss_llm::{JsonCompletionRequest, LlmError, LlmProvider};
use thiserror::Error;
use uuid::Uuid;

use crate::dto::{ExtractedEvent, ExtractedEvents};
use crate::prompt::{retry_user_prompt, system_prompt};

#[derive(Debug, Error)]
pub enum NluError {
    #[error("LLM request failed: {0}")]
    Llm(#[from] LlmError),
    #[error("could not understand the message after retries - last response: {raw}")]
    Unparseable { raw: String },
}

/// Stopwords filtered out when turning free text into keywords for
/// lightweight document retrieval (SQLite FTS5 in guss-store). Deliberately
/// small and bilingual (en/id) rather than a full stopword list crate -
/// precision isn't critical since FTS5 ranking does the rest.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "for", "of", "to", "at", "in", "on", "is", "was", "were",
    "yang", "dan", "atau", "untuk", "dari", "ke", "di", "pada", "adalah",
];

/// Extracts rough keywords from an owner's message for document context
/// retrieval - callers pass these into `Store::search_documents` before
/// calling `Nlu::extract`.
pub fn extract_keywords(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() > 2 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

pub struct Nlu<P: LlmProvider> {
    provider: P,
    language: String,
    max_retries: u32,
}

impl<P: LlmProvider> Nlu<P> {
    pub fn new(provider: P, language: impl Into<String>) -> Self {
        Self {
            provider,
            language: language.into(),
            max_retries: 2,
        }
    }

    /// The events Guss could fully work out; anything with a problem
    /// (see `extract_checked`) is left out.
    pub async fn extract(
        &self,
        business_id: Uuid,
        source_channel: SourceChannel,
        text: &str,
        default_currency: &str,
        context_snippets: &[String],
    ) -> Result<Vec<BusinessEvent>, NluError> {
        Ok(self
            .extract_checked(
                business_id,
                source_channel,
                text,
                default_currency,
                context_snippets,
            )
            .await?
            .events)
    }

    /// Like `extract`, but also returns what Guss couldn't work out (a
    /// stated total that disagrees with quantity x price, a missing or
    /// non-positive price). Callers should save nothing from a message
    /// with issues and tell the owner instead.
    pub async fn extract_checked(
        &self,
        business_id: Uuid,
        source_channel: SourceChannel,
        text: &str,
        default_currency: &str,
        context_snippets: &[String],
    ) -> Result<Extraction, NluError> {
        let schema_value = serde_json::to_value(schemars::schema_for!(ExtractedEvents))
            .expect("ExtractedEvents schema always serializes");
        let system = system_prompt(&self.language, context_snippets);
        let mut user_prompt = text.to_string();

        for attempt in 0..=self.max_retries {
            let request = JsonCompletionRequest {
                system_prompt: system.clone(),
                user_prompt: user_prompt.clone(),
                schema_name: "extracted_events".to_string(),
                json_schema: schema_value.clone(),
                temperature: 0.0,
            };

            let value = self.provider.complete_json(request).await?;

            match serde_json::from_value::<ExtractedEvents>(value.clone()) {
                Ok(dto) => {
                    let mut out = Extraction {
                        question: dto.question,
                        days_ago: dto.days_ago.filter(|d| *d > 0),
                        ..Extraction::default()
                    };
                    for e in dto.events {
                        match to_business_event(
                            e,
                            business_id,
                            source_channel,
                            text,
                            default_currency,
                        ) {
                            Ok(event) => out.events.push(event),
                            Err(issue) => out.issues.push(issue),
                        }
                    }
                    return Ok(out);
                }
                Err(parse_err) => {
                    if attempt == self.max_retries {
                        return Err(NluError::Unparseable {
                            raw: value.to_string(),
                        });
                    }
                    tracing::warn!(attempt, %parse_err, "LLM output failed schema validation, retrying");
                    user_prompt = retry_user_prompt(
                        text,
                        &value.to_string(),
                        &parse_err.to_string(),
                        &self.language,
                    );
                }
            }
        }

        unreachable!("loop always returns within max_retries + 1 iterations")
    }
}

/// Everything one message produced: what can be saved, and what Guss
/// couldn't work out.
#[derive(Debug, Default)]
pub struct Extraction {
    pub events: Vec<BusinessEvent>,
    pub issues: Vec<Issue>,
    /// The message asked something instead (answered, never stored).
    pub question: Option<crate::dto::Question>,
    /// "yesterday" -> 1: the events happened that many days ago (the
    /// caller dates them in the shop's time zone).
    pub days_ago: Option<u32>,
}

/// Something in a message that Guss must not guess at.
#[derive(Debug, Clone, PartialEq)]
pub enum Issue {
    /// "30 x 650 = 18500": the stated total disagrees with quantity x price.
    TotalMismatch {
        what: String,
        quantity: f64,
        each: f64,
        stated_total: f64,
    },
    /// A sale or expense with no amount at all.
    MissingAmount { what: String },
    /// Zero or negative price, amount or quantity.
    NotPositive { what: String, value: f64 },
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        format!("{n:.2}")
    }
}

impl Issue {
    pub fn message(&self, language: &str) -> String {
        match (self, language) {
            (
                Issue::TotalMismatch {
                    what,
                    quantity,
                    each,
                    stated_total,
                },
                lang,
            ) => {
                let (q, e, s, t) = (
                    fmt_num(*quantity),
                    fmt_num(*each),
                    fmt_num(*stated_total),
                    fmt_num(quantity * each),
                );
                if lang == "id" {
                    format!("{what}: {q} x {e} = {t}, bukan {s}. Mana yang benar?")
                } else {
                    format!("{what}: {q} x {e} = {t}, not {s}. Which is right?")
                }
            }
            (Issue::MissingAmount { what }, "id") => format!("{what}: berapa harganya?"),
            (Issue::MissingAmount { what }, _) => format!("{what}: how much was it?"),
            (Issue::NotPositive { what, value }, "id") => {
                format!(
                    "{what}: {} tidak mungkin, harus lebih dari 0.",
                    fmt_num(*value)
                )
            }
            (Issue::NotPositive { what, value }, _) => {
                format!(
                    "{what}: {} can't be right, it must be more than 0.",
                    fmt_num(*value)
                )
            }
        }
    }
}

/// Unit and total from what was said. The model never does arithmetic;
/// this does, and refuses to pick a side when the two disagree.
fn resolve(
    what: &str,
    quantity: f64,
    each: Option<f64>,
    total: Option<f64>,
) -> Result<Option<f64>, Issue> {
    let not_positive = |value: f64| Issue::NotPositive {
        what: what.to_string(),
        value,
    };
    if let Some(v) = each.into_iter().chain(total).find(|v| *v <= 0.0) {
        return Err(not_positive(v));
    }
    match (each, total) {
        (Some(each), Some(total)) => {
            let computed = quantity * each;
            // Half a percent, or one shilling/cent, of slack for rounding.
            if (computed - total).abs() > (total * 0.005).max(1.0) {
                return Err(Issue::TotalMismatch {
                    what: what.to_string(),
                    quantity,
                    each,
                    stated_total: total,
                });
            }
            Ok(Some(each))
        }
        (Some(each), None) => Ok(Some(each)),
        (None, Some(total)) if quantity > 0.0 => Ok(Some(total / quantity)),
        (None, Some(total)) => Err(not_positive(total)),
        (None, None) => Ok(None),
    }
}

fn to_business_event(
    event: ExtractedEvent,
    business_id: Uuid,
    source_channel: SourceChannel,
    raw_text: &str,
    default_currency: &str,
) -> Result<BusinessEvent, Issue> {
    let now = Utc::now();
    Ok(match event {
        ExtractedEvent::Sale {
            item_name,
            quantity,
            unit,
            unit_price_major,
            total_major,
            currency,
            category,
            paid_by,
            credit_to,
        } => {
            if quantity <= 0.0 {
                return Err(Issue::NotPositive {
                    what: item_name,
                    value: quantity,
                });
            }
            let each =
                resolve(&item_name, quantity, unit_price_major, total_major)?.ok_or_else(|| {
                    Issue::MissingAmount {
                        what: item_name.clone(),
                    }
                })?;
            BusinessEvent::Sale(Sale {
                id: Uuid::new_v4(),
                business_id,
                item_name,
                quantity,
                unit,
                unit_factor: 1.0,
                unit_price: CurrencyAmount::new(
                    each,
                    currency.unwrap_or_else(|| default_currency.to_string()),
                ),
                category,
                occurred_at: now,
                recurring: None,
                source_channel,
                paid_by: paid_by.map(|m| m.as_str().to_string()),
                credit_to: credit_to
                    .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|c| !c.is_empty()),
                returns: None,
                raw_text: Some(raw_text.to_string()),
            })
        }
        ExtractedEvent::Expense {
            description,
            amount_major,
            quantity,
            unit_amount_major,
            currency,
            category,
        } => {
            let qty = quantity.unwrap_or(1.0);
            let each =
                resolve(&description, qty, unit_amount_major, amount_major)?.ok_or_else(|| {
                    Issue::MissingAmount {
                        what: description.clone(),
                    }
                })?;
            BusinessEvent::Expense(Expense {
                id: Uuid::new_v4(),
                business_id,
                description,
                amount: CurrencyAmount::new(
                    qty * each,
                    currency.unwrap_or_else(|| default_currency.to_string()),
                ),
                category,
                occurred_at: now,
                recurring: None,
                source_channel,
                raw_text: Some(raw_text.to_string()),
            })
        }
        ExtractedEvent::StockUpdate {
            item_name,
            delta,
            unit,
            unit_cost_major,
            total_cost_major,
            reorder_threshold,
        } => {
            let cost = resolve(&item_name, delta.abs(), unit_cost_major, total_cost_major)?;
            BusinessEvent::StockUpdate(StockUpdate {
                id: Uuid::new_v4(),
                business_id,
                item_name,
                delta,
                unit,
                unit_factor: 1.0,
                unit_cost: cost.map(|c| CurrencyAmount::new(c, default_currency.to_string())),
                reorder_threshold,
                occurred_at: now,
                source_channel,
                raw_text: Some(raw_text.to_string()),
            })
        }
        // The price and the sale it gives back are filled in from the
        // books before it's saved (orchestrator), not guessed here.
        ExtractedEvent::Return {
            item_name,
            quantity,
            unit,
        } => {
            if quantity <= 0.0 {
                return Err(Issue::NotPositive {
                    what: item_name,
                    value: quantity,
                });
            }
            BusinessEvent::Sale(Sale {
                id: Uuid::new_v4(),
                business_id,
                item_name,
                quantity: -quantity,
                unit,
                unit_factor: 1.0,
                unit_price: CurrencyAmount::new(0.0, default_currency.to_string()),
                category: None,
                occurred_at: now,
                recurring: None,
                source_channel,
                paid_by: None,
                credit_to: None,
                returns: None,
                raw_text: Some(raw_text.to_string()),
            })
        }
        ExtractedEvent::DebtPayment {
            customer,
            amount_major,
            currency,
            paid_by,
        } => {
            if amount_major <= 0.0 {
                return Err(Issue::NotPositive {
                    what: customer,
                    value: amount_major,
                });
            }
            BusinessEvent::DebtPayment(DebtPayment {
                id: Uuid::new_v4(),
                business_id,
                customer: customer.split_whitespace().collect::<Vec<_>>().join(" "),
                amount: CurrencyAmount::new(
                    amount_major,
                    currency.unwrap_or_else(|| default_currency.to_string()),
                ),
                paid_by: paid_by.map(|m| m.as_str().to_string()),
                occurred_at: now,
                source_channel,
                raw_text: Some(raw_text.to_string()),
            })
        }
        ExtractedEvent::Note { text } => BusinessEvent::Note(Note {
            id: Uuid::new_v4(),
            business_id,
            text,
            occurred_at: now,
            source_channel,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ScriptedProvider {
        responses: Mutex<Vec<Result<Value, LlmError>>>,
        calls: AtomicU32,
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn complete_json(&self, _req: JsonCompletionRequest) -> Result<Value, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses.lock().unwrap().remove(0)
        }
    }

    #[tokio::test]
    async fn valid_response_extracts_one_sale() {
        let provider = ScriptedProvider {
            responses: Mutex::new(vec![Ok(json!({
                "events": [
                    {"kind": "sale", "item_name": "rice", "quantity": 3.0, "unit_price_major": 500.0, "currency": null, "category": null}
                ]
            }))]),
            calls: AtomicU32::new(0),
        };
        let nlu = Nlu::new(provider, "en");

        let events = nlu
            .extract(
                Uuid::new_v4(),
                SourceChannel::Cli,
                "sold 3 bags of rice at 500 each",
                "USD",
                &[],
            )
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            BusinessEvent::Sale(s) => {
                assert_eq!(s.item_name, "rice");
                assert_eq!(s.quantity, 3.0);
                assert_eq!(s.unit_price.amount.minor_units, 50000);
            }
            other => panic!("expected Sale, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retries_once_then_succeeds_on_malformed_first_response() {
        let provider = ScriptedProvider {
            responses: Mutex::new(vec![
                Ok(json!({"not_events": "oops"})),
                Ok(json!({"events": [{"kind": "note", "text": "owner mentioned slow day"}]})),
            ]),
            calls: AtomicU32::new(0),
        };
        let nlu = Nlu::new(provider, "en");

        let events = nlu
            .extract(
                Uuid::new_v4(),
                SourceChannel::Cli,
                "slow day today",
                "USD",
                &[],
            )
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], BusinessEvent::Note(_)));
    }

    #[tokio::test]
    async fn exhausted_retries_surface_unparseable_error() {
        let provider = ScriptedProvider {
            responses: Mutex::new(vec![
                Ok(json!({"garbage": 1})),
                Ok(json!({"garbage": 2})),
                Ok(json!({"garbage": 3})),
            ]),
            calls: AtomicU32::new(0),
        };
        let nlu = Nlu::new(provider, "en");

        let err = nlu
            .extract(Uuid::new_v4(), SourceChannel::Cli, "???", "USD", &[])
            .await
            .unwrap_err();

        assert!(matches!(err, NluError::Unparseable { .. }));
    }

    fn sale_json(each: Option<f64>, total: Option<f64>, qty: f64) -> Value {
        json!({"events": [{"kind": "sale", "item_name": "rice", "quantity": qty,
            "unit_price_major": each, "total_major": total, "currency": null, "category": null}]})
    }

    async fn checked(response: Value) -> Extraction {
        let provider = ScriptedProvider {
            responses: Mutex::new(vec![Ok(response)]),
            calls: AtomicU32::new(0),
        };
        Nlu::new(provider, "en")
            .extract_checked(Uuid::nil(), SourceChannel::Cli, "x", "KES", &[])
            .await
            .unwrap()
    }

    fn only_sale(x: &Extraction) -> &Sale {
        match &x.events[..] {
            [BusinessEvent::Sale(s)] => s,
            other => panic!("expected one sale, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn how_the_customer_paid_is_kept_only_when_said() {
        let mut said = sale_json(Some(60.0), None, 2.0);
        said["events"][0]["paid_by"] = json!("mpesa");
        assert_eq!(
            only_sale(&checked(said).await).paid_by.as_deref(),
            Some("mpesa")
        );
        // Older replies without the field, and an explicit null: not said.
        assert_eq!(
            only_sale(&checked(sale_json(Some(60.0), None, 2.0)).await).paid_by,
            None
        );
        let mut null = sale_json(Some(60.0), None, 2.0);
        null["events"][0]["paid_by"] = Value::Null;
        assert_eq!(only_sale(&checked(null).await).paid_by, None);
    }

    #[tokio::test]
    async fn total_only_is_divided_by_code_not_the_model() {
        let x = checked(sale_json(None, Some(360.0), 3.0)).await;
        assert!(x.issues.is_empty());
        assert_eq!(only_sale(&x).unit_price.major_units(), 120.0);
        assert_eq!(only_sale(&x).total().major_units(), 360.0);
    }

    #[tokio::test]
    async fn agreeing_unit_and_total_pass() {
        let x = checked(sale_json(Some(120.0), Some(360.0), 3.0)).await;
        assert!(x.issues.is_empty());
        assert_eq!(only_sale(&x).unit_price.major_units(), 120.0);
    }

    #[tokio::test]
    async fn disagreeing_total_is_an_issue_and_nothing_is_kept() {
        let x = checked(
            json!({"events": [{"kind": "stock_update", "item_name": "tshirt",
            "delta": 30.0, "unit": null, "unit_cost_major": 650.0, "total_cost_major": 18500.0,
            "reorder_threshold": null}]}),
        )
        .await;
        assert!(x.events.is_empty());
        assert_eq!(
            x.issues[0].message("en"),
            "tshirt: 30 x 650 = 19500, not 18500. Which is right?"
        );
        assert_eq!(
            x.issues[0].message("id"),
            "tshirt: 30 x 650 = 19500, bukan 18500. Mana yang benar?"
        );
    }

    #[tokio::test]
    async fn missing_zero_and_negative_prices_are_issues() {
        let x = checked(sale_json(None, None, 3.0)).await;
        assert_eq!(
            x.issues,
            vec![Issue::MissingAmount {
                what: "rice".into()
            }]
        );
        let x = checked(sale_json(Some(0.0), None, 3.0)).await;
        assert!(matches!(x.issues[0], Issue::NotPositive { .. }));
        let x = checked(sale_json(Some(-120.0), None, 3.0)).await;
        assert!(matches!(x.issues[0], Issue::NotPositive { .. }));
    }

    #[tokio::test]
    async fn expense_from_count_and_price_each() {
        let x = checked(
            json!({"events": [{"kind": "expense", "description": "staff t-shirts",
            "amount_major": null, "quantity": 30.0, "unit_amount_major": 650.0,
            "currency": null, "category": null}]}),
        )
        .await;
        match &x.events[..] {
            [BusinessEvent::Expense(e)] => assert_eq!(e.amount.major_units(), 19500.0),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn restock_cost_from_total() {
        let x = checked(
            json!({"events": [{"kind": "stock_update", "item_name": "soda",
            "delta": 2.0, "unit": "crate", "unit_cost_major": null, "total_cost_major": 2400.0,
            "reorder_threshold": null}]}),
        )
        .await;
        match &x.events[..] {
            [BusinessEvent::StockUpdate(su)] => {
                assert_eq!(su.unit_cost.as_ref().unwrap().major_units(), 1200.0)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keyword_extraction_drops_stopwords_and_punctuation() {
        let keywords = extract_keywords("What is the price of rice, and soap?");
        assert_eq!(keywords, vec!["what", "price", "rice", "soap"]);
    }
}
