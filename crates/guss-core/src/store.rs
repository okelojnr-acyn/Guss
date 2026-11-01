use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{BusinessEvent, BusinessProfile, Document};

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

/// Storage abstraction implemented by guss-store (SQLite by default,
/// Postgres optional). Business logic never depends on the concrete
/// database - only on this trait.
#[async_trait]
pub trait Store: Send + Sync {
    async fn save_event(&self, event: &BusinessEvent) -> Result<(), StoreError>;
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

    async fn save_business_profile(&self, profile: &BusinessProfile) -> Result<(), StoreError>;
    async fn load_business_profile(&self) -> Result<Option<BusinessProfile>, StoreError>;

    /// Idempotency guard: records a provider message id as seen, returning
    /// `true` if it was newly recorded (process it) or `false` if it had
    /// already been seen (a webhook redelivery - skip it).
    async fn mark_message_processed(&self, source: &str, provider_message_id: &str)
        -> Result<bool, StoreError>;
}
