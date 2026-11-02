use guss_core::domain::{BusinessEvent, RecurrenceInterval, SourceChannel};

/// Dialect-agnostic flattening of a `BusinessEvent` into the `events`
/// table's columns - shared by `sqlite.rs` and `postgres.rs` so the two
/// backends can't drift apart on what a given event maps to.
pub struct EventColumns {
    pub id: String,
    pub business_id: String,
    pub kind: &'static str,
    pub occurred_at: String,
    pub item_name: Option<String>,
    pub quantity: Option<f64>,
    pub delta: Option<f64>,
    pub unit: Option<String>,
    pub amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub reorder_threshold: Option<f64>,
    pub recurring_interval: Option<&'static str>,
    pub recurring_label: Option<String>,
    pub source_channel: &'static str,
    pub raw_text: Option<String>,
    pub payload: String,
}

pub fn source_channel_str(c: SourceChannel) -> &'static str {
    match c {
        SourceChannel::Cli => "cli",
        SourceChannel::Telegram => "telegram",
        SourceChannel::WhatsApp => "whatsapp",
    }
}

pub fn recurrence_interval_str(i: RecurrenceInterval) -> &'static str {
    match i {
        RecurrenceInterval::Daily => "daily",
        RecurrenceInterval::Weekly => "weekly",
        RecurrenceInterval::Monthly => "monthly",
    }
}

pub fn columns_for(event: &BusinessEvent) -> EventColumns {
    let payload = serde_json::to_string(event).expect("BusinessEvent always serializes");
    match event {
        BusinessEvent::Sale(s) => EventColumns {
            id: s.id.to_string(),
            business_id: s.business_id.to_string(),
            kind: "sale",
            occurred_at: s.occurred_at.to_rfc3339(),
            item_name: Some(s.item_name.clone()),
            quantity: Some(s.quantity),
            delta: None,
            unit: None,
            amount_minor: Some(s.unit_price.amount.minor_units),
            currency: Some(s.unit_price.currency.clone()),
            category: s.category.clone(),
            description: None,
            reorder_threshold: None,
            recurring_interval: s
                .recurring
                .as_ref()
                .map(|r| recurrence_interval_str(r.interval)),
            recurring_label: s.recurring.as_ref().map(|r| r.label.clone()),
            source_channel: source_channel_str(s.source_channel),
            raw_text: s.raw_text.clone(),
            payload,
        },
        BusinessEvent::Expense(e) => EventColumns {
            id: e.id.to_string(),
            business_id: e.business_id.to_string(),
            kind: "expense",
            occurred_at: e.occurred_at.to_rfc3339(),
            item_name: None,
            quantity: None,
            delta: None,
            unit: None,
            amount_minor: Some(e.amount.amount.minor_units),
            currency: Some(e.amount.currency.clone()),
            category: e.category.clone(),
            description: Some(e.description.clone()),
            reorder_threshold: None,
            recurring_interval: e
                .recurring
                .as_ref()
                .map(|r| recurrence_interval_str(r.interval)),
            recurring_label: e.recurring.as_ref().map(|r| r.label.clone()),
            source_channel: source_channel_str(e.source_channel),
            raw_text: e.raw_text.clone(),
            payload,
        },
        BusinessEvent::StockUpdate(su) => EventColumns {
            id: su.id.to_string(),
            business_id: su.business_id.to_string(),
            kind: "stock_update",
            occurred_at: su.occurred_at.to_rfc3339(),
            item_name: Some(su.item_name.clone()),
            quantity: None,
            delta: Some(su.delta),
            unit: su.unit.clone(),
            amount_minor: su.unit_cost.as_ref().map(|c| c.amount.minor_units),
            currency: su.unit_cost.as_ref().map(|c| c.currency.clone()),
            category: None,
            description: None,
            reorder_threshold: su.reorder_threshold,
            recurring_interval: None,
            recurring_label: None,
            source_channel: source_channel_str(su.source_channel),
            raw_text: su.raw_text.clone(),
            payload,
        },
        BusinessEvent::Note(n) => EventColumns {
            id: n.id.to_string(),
            business_id: n.business_id.to_string(),
            kind: "note",
            occurred_at: n.occurred_at.to_rfc3339(),
            item_name: None,
            quantity: None,
            delta: None,
            unit: None,
            amount_minor: None,
            currency: None,
            category: None,
            description: Some(n.text.clone()),
            reorder_threshold: None,
            recurring_interval: None,
            recurring_label: None,
            source_channel: source_channel_str(n.source_channel),
            raw_text: None,
            payload,
        },
    }
}
