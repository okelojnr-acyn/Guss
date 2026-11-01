use chrono::Utc;
use guss_core::domain::{BusinessEvent, Expense, Note, Sale, SourceChannel, StockUpdate};
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
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
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

    pub async fn extract(
        &self,
        business_id: Uuid,
        source_channel: SourceChannel,
        text: &str,
        default_currency: &str,
        context_snippets: &[String],
    ) -> Result<Vec<BusinessEvent>, NluError> {
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
                    return Ok(dto
                        .events
                        .into_iter()
                        .map(|e| to_business_event(e, business_id, source_channel, text, default_currency))
                        .collect());
                }
                Err(parse_err) => {
                    if attempt == self.max_retries {
                        return Err(NluError::Unparseable { raw: value.to_string() });
                    }
                    tracing::warn!(attempt, %parse_err, "LLM output failed schema validation, retrying");
                    user_prompt = retry_user_prompt(text, &value.to_string(), &parse_err.to_string(), &self.language);
                }
            }
        }

        unreachable!("loop always returns within max_retries + 1 iterations")
    }
}

fn to_business_event(
    event: ExtractedEvent,
    business_id: Uuid,
    source_channel: SourceChannel,
    raw_text: &str,
    default_currency: &str,
) -> BusinessEvent {
    let now = Utc::now();
    match event {
        ExtractedEvent::Sale { item_name, quantity, unit_price_major, currency, category } => {
            BusinessEvent::Sale(Sale {
                id: Uuid::new_v4(),
                business_id,
                item_name,
                quantity,
                unit_price: CurrencyAmount::new(unit_price_major, currency.unwrap_or_else(|| default_currency.to_string())),
                category,
                occurred_at: now,
                recurring: None,
                source_channel,
                raw_text: Some(raw_text.to_string()),
            })
        }
        ExtractedEvent::Expense { description, amount_major, currency, category } => {
            BusinessEvent::Expense(Expense {
                id: Uuid::new_v4(),
                business_id,
                description,
                amount: CurrencyAmount::new(amount_major, currency.unwrap_or_else(|| default_currency.to_string())),
                category,
                occurred_at: now,
                recurring: None,
                source_channel,
                raw_text: Some(raw_text.to_string()),
            })
        }
        ExtractedEvent::StockUpdate { item_name, delta, unit, unit_cost_major, reorder_threshold } => {
            BusinessEvent::StockUpdate(StockUpdate {
                id: Uuid::new_v4(),
                business_id,
                item_name,
                delta,
                unit,
                unit_cost: unit_cost_major.map(|c| CurrencyAmount::new(c, default_currency.to_string())),
                reorder_threshold,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

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
            .extract(Uuid::new_v4(), SourceChannel::Cli, "sold 3 bags of rice at 500 each", "USD", &[])
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
            .extract(Uuid::new_v4(), SourceChannel::Cli, "slow day today", "USD", &[])
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

    #[test]
    fn keyword_extraction_drops_stopwords_and_punctuation() {
        let keywords = extract_keywords("What is the price of rice, and soap?");
        assert_eq!(keywords, vec!["what", "price", "rice", "soap"]);
    }
}
