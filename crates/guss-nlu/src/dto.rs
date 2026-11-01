use schemars::JsonSchema;
use serde::Deserialize;

/// What the LLM is asked to return: one event per clause in the owner's
/// utterance (e.g. "sold 3 bags of rice and paid 500 for transport" ->
/// two events). Monetary amounts are "major" units (e.g. 12.50, not minor
/// units) since that's how a human/LLM naturally reasons about them -
/// guss-nlu converts to Money (minor units) when mapping to BusinessEvent.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtractedEvent {
    Sale {
        item_name: String,
        quantity: f64,
        unit_price_major: f64,
        /// ISO 4217 code, e.g. "USD". Omit to use the business's default currency.
        currency: Option<String>,
        category: Option<String>,
    },
    Expense {
        description: String,
        amount_major: f64,
        currency: Option<String>,
        category: Option<String>,
    },
    StockUpdate {
        item_name: String,
        /// Positive for restock, negative for a write-off/adjustment.
        delta: f64,
        unit: Option<String>,
        unit_cost_major: Option<f64>,
        reorder_threshold: Option<f64>,
    },
    Note {
        text: String,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExtractedEvents {
    pub events: Vec<ExtractedEvent>,
}
