use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{BusinessEvent, BusinessProfile, Document, Item};
use crate::people::{Actor, Role};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(String),
    #[error("not found")]
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeWindow {
    Day,
    Week,
    Month,
}

impl TimeWindow {
    /// Inclusive start timestamp for this window ending now, in UTC.
    pub fn start(self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            TimeWindow::Day => now - chrono::Duration::days(1),
            TimeWindow::Week => now - chrono::Duration::days(7),
            TimeWindow::Month => now - chrono::Duration::days(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StockLevel {
    pub item_name: String,
    pub current_quantity: f64,
    pub reorder_threshold: Option<f64>,
}

/// One entry in the journal: an event with who recorded it and, if it no
/// longer counts, who voided it and why. Nothing is ever deleted.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    /// Short number people use to refer to it ("/void 12").
    pub number: i64,
    pub event: BusinessEvent,
    pub recorded_by: Option<String>,
    pub recorded_by_role: Option<Role>,
    pub voided: Option<Voided>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Voided {
    pub by: String,
    pub reason: String,
    pub at: DateTime<Utc>,
}

/// Someone asked for an entry to stop counting; someone above decides.
#[derive(Debug, Clone, PartialEq)]
pub struct VoidRequest {
    pub number: i64,
    pub entry: i64,
    pub requested_by_id: String,
    pub requested_by: String,
    pub reason: String,
    pub requested_at: DateTime<Utc>,
}

/// Storage abstraction implemented by guss-store (SQLite by default,
/// Postgres optional). Business logic never depends on the concrete
/// database - only on this trait.
#[async_trait]
pub trait Store: Send + Sync {
    /// Saves an event with nobody recorded as its author.
    async fn save_event(&self, event: &BusinessEvent) -> Result<(), StoreError> {
        self.save_event_by(event, None).await
    }
    async fn save_event_by(
        &self,
        event: &BusinessEvent,
        author: Option<&Actor>,
    ) -> Result<(), StoreError>;
    /// Marks an event as no longer counting (totals, stock, reports skip
    /// it). Nothing is deleted: the entry, its author and the void stay in
    /// the journal. Voiding an already voided event changes nothing.
    async fn void_event(&self, id: Uuid, by: &Actor, reason: &str) -> Result<(), StoreError>;
    /// Newest first, voided entries included.
    async fn journal(&self, business_id: Uuid, limit: i64)
    -> Result<Vec<JournalEntry>, StoreError>;
    async fn journal_entry(
        &self,
        business_id: Uuid,
        number: i64,
    ) -> Result<Option<JournalEntry>, StoreError>;

    /// Returns the request's number.
    async fn request_void(
        &self,
        business_id: Uuid,
        entry: i64,
        by: &Actor,
        reason: &str,
    ) -> Result<i64, StoreError>;
    async fn pending_void_requests(
        &self,
        business_id: Uuid,
    ) -> Result<Vec<VoidRequest>, StoreError>;
    /// Closes a pending request (approved or not); returns it, or None if
    /// there is no such pending request.
    async fn decide_void_request(
        &self,
        business_id: Uuid,
        number: i64,
        by: &Actor,
        approved: bool,
    ) -> Result<Option<VoidRequest>, StoreError>;
    async fn list_events(
        &self,
        business_id: Uuid,
        window: TimeWindow,
    ) -> Result<Vec<BusinessEvent>, StoreError>;
    async fn current_stock(&self, business_id: Uuid, item_name: &str) -> Result<f64, StoreError>;
    async fn low_stock_items(&self, business_id: Uuid) -> Result<Vec<StockLevel>, StoreError>;

    async fn save_document(&self, doc: &Document) -> Result<(), StoreError>;
    async fn search_documents(
        &self,
        business_id: Uuid,
        terms: &[String],
        limit: i64,
    ) -> Result<Vec<Document>, StoreError>;

    /// The shop's items and their units (insert or replace by name).
    async fn save_item(&self, business_id: Uuid, item: &Item) -> Result<(), StoreError>;
    async fn list_items(&self, business_id: Uuid) -> Result<Vec<Item>, StoreError>;

    async fn save_business_profile(&self, profile: &BusinessProfile) -> Result<(), StoreError>;
    async fn load_business_profile(&self) -> Result<Option<BusinessProfile>, StoreError>;

    /// Idempotency guard: records a provider message id as seen, returning
    /// `true` if it was newly recorded (process it) or `false` if it had
    /// already been seen (a webhook redelivery - skip it).
    async fn mark_message_processed(
        &self,
        source: &str,
        provider_message_id: &str,
    ) -> Result<bool, StoreError>;
}
