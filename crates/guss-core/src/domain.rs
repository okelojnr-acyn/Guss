use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::money::CurrencyAmount;

/// How a recurring sale/expense should be normalized onto a monthly basis
/// for the simple MRR-style projection in guss-insights. This is explicitly
/// a projection, not real subscription-cohort MRR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecurrenceInterval {
    Daily,
    Weekly,
    Monthly,
}

impl RecurrenceInterval {
    /// Multiplier to normalize an amount at this interval onto a monthly figure.
    pub fn monthly_factor(self) -> f64 {
        match self {
            RecurrenceInterval::Daily => 30.0,
            RecurrenceInterval::Weekly => 4.33,
            RecurrenceInterval::Monthly => 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecurrenceInfo {
    pub interval: RecurrenceInterval,
    /// Human label grouping recurring line items, e.g. "Acme Corp retainer".
    pub label: String,
}

/// Identifies which inbound channel produced an event, for audit/debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceChannel {
    Cli,
    Telegram,
    WhatsApp,
    Web,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sale {
    pub id: Uuid,
    pub business_id: Uuid,
    pub item_name: String,
    /// How many, in `unit` (as said: "2 crates" -> 2.0, Some("crate")).
    pub quantity: f64,
    /// The unit said; None means the item's base unit.
    #[serde(default)]
    pub unit: Option<String>,
    /// Base units per `unit` (a crate of 24 bottles -> 24). Stock counts
    /// `quantity * unit_factor`; money never goes through the factor.
    #[serde(default = "one")]
    pub unit_factor: f64,
    /// Price of one `unit` (one crate, not one bottle).
    pub unit_price: CurrencyAmount,
    pub category: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub recurring: Option<RecurrenceInfo>,
    pub source_channel: SourceChannel,
    /// How the customer paid, when it was said: "cash", "mpesa",
    /// "airtel", "card" or "bank". None = not said (Friday books cash).
    #[serde(default)]
    pub paid_by: Option<String>,
    /// Sold on credit to this customer ("sold to John on credit"): they
    /// owe it until a DebtPayment. None = paid now.
    #[serde(default)]
    pub credit_to: Option<String>,
    /// A return: `quantity` is negative and this is the sale it gives
    /// back ("returned 1 rice" -> the latest rice sale).
    #[serde(default)]
    pub returns: Option<Uuid>,
    /// Original utterance this was extracted from, kept for audit/debug.
    pub raw_text: Option<String>,
}

fn one() -> f64 {
    1.0
}

impl Sale {
    /// Quantity in the item's base unit.
    pub fn base_quantity(&self) -> f64 {
        self.quantity * self.unit_factor
    }

    /// Total = quantity * unit price, computed directly in minor units so
    /// fractional quantities (e.g. 1.5 kg) never round-trip through a
    /// string/float conversion.
    pub fn total(&self) -> CurrencyAmount {
        let total_minor =
            (self.quantity * self.unit_price.amount.minor_units as f64).round() as i64;
        CurrencyAmount {
            amount: crate::money::Money::from_minor_units(total_minor),
            currency: self.unit_price.currency.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expense {
    pub id: Uuid,
    pub business_id: Uuid,
    pub description: String,
    pub amount: CurrencyAmount,
    pub category: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub recurring: Option<RecurrenceInfo>,
    pub source_channel: SourceChannel,
    pub raw_text: Option<String>,
    /// Who paid: Some(true) the person who recorded it used their own
    /// money and wants it back; Some(false) the business paid; None not
    /// said (the business, for the owner; Guss asks anyone else).
    #[serde(default)]
    pub reimburse: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StockUpdate {
    pub id: Uuid,
    pub business_id: Uuid,
    pub item_name: String,
    /// Positive for restock, negative for manual write-off/adjustment.
    /// Sales are NOT double-counted here - current stock is derived at
    /// query time as SUM(StockUpdate.delta) - SUM(Sale.quantity).
    pub delta: f64,
    /// The unit said; None means the item's base unit.
    pub unit: Option<String>,
    /// Base units per `unit`, as for `Sale::unit_factor`.
    #[serde(default = "one")]
    pub unit_factor: f64,
    /// Cost of one `unit`.
    pub unit_cost: Option<CurrencyAmount>,
    pub reorder_threshold: Option<f64>,
    pub occurred_at: DateTime<Utc>,
    pub source_channel: SourceChannel,
    pub raw_text: Option<String>,
}

impl StockUpdate {
    /// Change in the item's base unit.
    pub fn base_delta(&self) -> f64 {
        self.delta * self.unit_factor
    }
}

/// Something the shop sells or stocks, with the units it is counted in.
/// Names are lower-case and singular; `aliases` catch other names for it
/// ("mchele" for rice, "tee" for tshirt).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    /// What stock is counted in ("bottle"). None until a unit is first used.
    pub base_unit: Option<String>,
    /// Other units and how many base units each holds: ("crate", 24.0).
    #[serde(default)]
    pub units: Vec<(String, f64)>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: Uuid,
    pub business_id: Uuid,
    pub text: String,
    pub occurred_at: DateTime<Utc>,
    pub source_channel: SourceChannel,
    /// Set on the note "/paylink John" leaves: the Paystack reference
    /// Guss checks with Friday until the customer pays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paystack_ref: Option<String>,
}

/// A customer paying what they owe for credit sales ("John paid 500").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebtPayment {
    pub id: Uuid,
    pub business_id: Uuid,
    pub customer: String,
    pub amount: CurrencyAmount,
    /// "cash", "mpesa", "airtel", "card" or "bank"; None = not said.
    #[serde(default)]
    pub paid_by: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub source_channel: SourceChannel,
    pub raw_text: Option<String>,
    /// Paid through a Paystack pay link: Friday recorded it when Paystack
    /// confirmed it, so it is never sent there again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paystack_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BusinessEvent {
    Sale(Sale),
    Expense(Expense),
    StockUpdate(StockUpdate),
    Note(Note),
    DebtPayment(DebtPayment),
}

impl BusinessEvent {
    pub fn id(&self) -> Uuid {
        match self {
            BusinessEvent::Sale(e) => e.id,
            BusinessEvent::Expense(e) => e.id,
            BusinessEvent::StockUpdate(e) => e.id,
            BusinessEvent::Note(e) => e.id,
            BusinessEvent::DebtPayment(e) => e.id,
        }
    }

    pub fn business_id(&self) -> Uuid {
        match self {
            BusinessEvent::Sale(e) => e.business_id,
            BusinessEvent::Expense(e) => e.business_id,
            BusinessEvent::StockUpdate(e) => e.business_id,
            BusinessEvent::Note(e) => e.business_id,
            BusinessEvent::DebtPayment(e) => e.business_id,
        }
    }

    pub fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            BusinessEvent::Sale(e) => e.occurred_at,
            BusinessEvent::Expense(e) => e.occurred_at,
            BusinessEvent::StockUpdate(e) => e.occurred_at,
            BusinessEvent::Note(e) => e.occurred_at,
            BusinessEvent::DebtPayment(e) => e.occurred_at,
        }
    }
}

/// A single-business profile for this Guss instance. Phase 1 is
/// single-business-per-instance; every event still carries a business_id
/// so a future multi-tenant mode doesn't require a schema migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessProfile {
    pub id: Uuid,
    pub name: String,
    pub default_currency: String,
    /// "en", "id" or "sw" - selects reply language, NLU prompt and report
    /// templates ("sw" uses the English prompt, which reads Swahili).
    pub language: String,
    pub timezone: String,
}

/// A reference document (price list, supplier notes, etc.) a business owner
/// uploads for extra NLU context, retrieved via lightweight keyword search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub business_id: Uuid,
    pub title: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::CurrencyAmount;

    fn sample_sale(quantity: f64, unit_price: f64) -> Sale {
        Sale {
            id: Uuid::new_v4(),
            business_id: Uuid::new_v4(),
            item_name: "rice".into(),
            quantity,
            unit: None,
            unit_factor: 1.0,
            unit_price: CurrencyAmount::new(unit_price, "USD"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            paid_by: None,
            credit_to: None,
            returns: None,
            raw_text: None,
        }
    }

    #[test]
    fn events_saved_before_units_still_load_with_factor_one() {
        let old = r#"{"kind":"Sale","id":"00000000-0000-0000-0000-000000000001",
            "business_id":"00000000-0000-0000-0000-000000000002","item_name":"rice",
            "quantity":3.0,"unit_price":{"amount":{"minor_units":12000},"currency":"KES"},
            "category":null,"occurred_at":"2026-11-14T10:00:00Z","recurring":null,
            "source_channel":"telegram","raw_text":null}"#;
        match serde_json::from_str::<BusinessEvent>(old).unwrap() {
            BusinessEvent::Sale(s) => {
                assert_eq!(s.unit, None);
                assert_eq!(s.base_quantity(), 3.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sale_total_multiplies_quantity_by_unit_price() {
        let sale = sample_sale(3.0, 5.0);
        assert_eq!(sale.total().amount.minor_units, 1500);
    }

    #[test]
    fn fractional_quantity_total_is_exact() {
        let sale = sample_sale(1.5, 10.0);
        assert_eq!(sale.total().amount.minor_units, 1500);
    }
}
