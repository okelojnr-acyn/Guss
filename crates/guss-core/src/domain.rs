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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sale {
    pub id: Uuid,
    pub business_id: Uuid,
    pub item_name: String,
    pub quantity: f64,
    pub unit_price: CurrencyAmount,
    pub category: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub recurring: Option<RecurrenceInfo>,
    pub source_channel: SourceChannel,
    /// Original utterance this was extracted from, kept for audit/debug.
    pub raw_text: Option<String>,
}

impl Sale {
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
    pub unit: Option<String>,
    pub unit_cost: Option<CurrencyAmount>,
    pub reorder_threshold: Option<f64>,
    pub occurred_at: DateTime<Utc>,
    pub source_channel: SourceChannel,
    pub raw_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: Uuid,
    pub business_id: Uuid,
    pub text: String,
    pub occurred_at: DateTime<Utc>,
    pub source_channel: SourceChannel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BusinessEvent {
    Sale(Sale),
    Expense(Expense),
    StockUpdate(StockUpdate),
    Note(Note),
}

impl BusinessEvent {
    pub fn id(&self) -> Uuid {
        match self {
            BusinessEvent::Sale(e) => e.id,
            BusinessEvent::Expense(e) => e.id,
            BusinessEvent::StockUpdate(e) => e.id,
            BusinessEvent::Note(e) => e.id,
        }
    }

    pub fn business_id(&self) -> Uuid {
        match self {
            BusinessEvent::Sale(e) => e.business_id,
            BusinessEvent::Expense(e) => e.business_id,
            BusinessEvent::StockUpdate(e) => e.business_id,
            BusinessEvent::Note(e) => e.business_id,
        }
    }

    pub fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            BusinessEvent::Sale(e) => e.occurred_at,
            BusinessEvent::Expense(e) => e.occurred_at,
            BusinessEvent::StockUpdate(e) => e.occurred_at,
            BusinessEvent::Note(e) => e.occurred_at,
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
    /// "en" or "id" - selects NLU prompt language and report templates.
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
            unit_price: CurrencyAmount::new(unit_price, "USD"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            raw_text: None,
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
