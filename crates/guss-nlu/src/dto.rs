use schemars::JsonSchema;
use serde::Deserialize;

/// What the LLM is asked to return: one event per clause in the owner's
/// utterance (e.g. "sold 3 bags of rice and paid 500 for transport" ->
/// two events). Monetary amounts are "major" units (e.g. 12.50, not minor
/// units) since that's how a human/LLM naturally reasons about them -
/// guss-nlu converts to Money (minor units) when mapping to BusinessEvent.
///
/// The model copies numbers exactly as said and never multiplies or
/// divides: a price "each" goes in the unit field, a price for the whole
/// lot in the total field, both when both were said. Guss's code does the
/// arithmetic and checks that the two agree (`extract::resolve`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtractedEvent {
    Sale {
        item_name: String,
        /// How many were sold, in `unit`. 1 if not said.
        quantity: f64,
        /// The unit the quantity is in, as said, singular ("crate", "kg", "bottle"). Null if none was said.
        unit: Option<String>,
        /// Price of ONE (one of `unit` if given), as said ("at 120", "120 each", "@120"). Null if not said.
        unit_price_major: Option<f64>,
        /// Price for ALL of them together, as said ("3 for 360"). Null if not said.
        total_major: Option<f64>,
        /// ISO 4217 code, e.g. "USD". Null to use the business's default currency.
        currency: Option<String>,
        category: Option<String>,
    },
    /// Money paid out for anything that is NOT stock bought to resell:
    /// rent, transport, fees, wages, subscriptions, and things for the
    /// shop's or staff's own use (uniforms, cleaning supplies, tea).
    Expense {
        description: String,
        /// Total paid, as said. Null if only a quantity and price each were said.
        amount_major: Option<f64>,
        /// How many, when the owner said a count and a price each ("30 shirts at 650 each"). Null otherwise.
        quantity: Option<f64>,
        /// Price of one, as said. Null if not said.
        unit_amount_major: Option<f64>,
        currency: Option<String>,
        category: Option<String>,
    },
    /// Stock received to resell (positive delta, usually with what was
    /// paid) or lost/spoiled/written off (negative delta, no cost).
    StockUpdate {
        item_name: String,
        /// Positive for restock, negative for a write-off/adjustment.
        delta: f64,
        unit: Option<String>,
        /// Cost of ONE unit, as said. Null if not said.
        unit_cost_major: Option<f64>,
        /// Cost of ALL of them together, as said. Null if not said.
        total_cost_major: Option<f64>,
        reorder_threshold: Option<f64>,
    },
    /// Business information that is not money or stock ("slow day
    /// today"). Never used for greetings, thanks or questions.
    Note { text: String },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExtractedEvents {
    pub events: Vec<ExtractedEvent>,
    /// Set when the message ASKS something about the books instead of
    /// reporting what happened (then events is empty). Null otherwise.
    #[serde(default)]
    pub question: Option<Question>,
    /// When it happened, if not today: 1 for "yesterday", 2 for "the day
    /// before yesterday", 3 for "on Monday" three days ago. Null for today.
    #[serde(default)]
    pub days_ago: Option<u32>,
}

/// A question Guss answers from its own books (never stored).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Question {
    /// "how many tshirts are left?", "sisa beras berapa?"
    StockLeft { item_name: String },
    /// "how much did I sell today?", "what did I spend on transport this
    /// week?": totals for a period, optionally about one item or expense.
    Totals {
        /// "today", "week" or "month".
        period: String,
        /// The item or expense asked about ("transport", "rice"). Null for everything.
        about: Option<String>,
    },
}
