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
    /// Rolling start (the last 24 h, 7 or 30 days), in UTC.
    pub fn start(self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            TimeWindow::Day => now - chrono::Duration::days(1),
            TimeWindow::Week => now - chrono::Duration::days(7),
            TimeWindow::Month => now - chrono::Duration::days(30),
        }
    }

    /// Start in the shop's own calendar: "today" begins at local midnight,
    /// "week" is today and the 6 days before, "month" today and the 29
    /// before. An unknown time zone name counts as UTC.
    pub fn start_in(self, now: DateTime<Utc>, timezone: &str) -> DateTime<Utc> {
        use chrono::{Days, TimeZone};
        let tz: chrono_tz::Tz = timezone.trim().parse().unwrap_or(chrono_tz::UTC);
        let today = now.with_timezone(&tz).date_naive();
        let back = match self {
            TimeWindow::Day => 0,
            TimeWindow::Week => 6,
            TimeWindow::Month => 29,
        };
        let first_day = today.checked_sub_days(Days::new(back)).unwrap_or(today);
        let midnight = first_day.and_hms_opt(0, 0, 0).expect("midnight exists");
        // Where midnight doesn't exist or happens twice (DST), take the
        // earliest instant that day.
        tz.from_local_datetime(&midnight)
            .earliest()
            .unwrap_or_else(|| tz.from_utc_datetime(&midnight))
            .with_timezone(&Utc)
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn today_starts_at_the_shops_midnight() {
        // 02:30 in Nairobi (UTC+3) is 23:30 UTC the day before.
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 23, 30, 0).unwrap();
        assert_eq!(
            TimeWindow::Day.start_in(now, "Africa/Nairobi"),
            Utc.with_ymd_and_hms(2026, 11, 14, 21, 0, 0).unwrap()
        );
        assert_eq!(
            TimeWindow::Day.start_in(now, "UTC"),
            Utc.with_ymd_and_hms(2026, 11, 14, 0, 0, 0).unwrap()
        );
        assert_eq!(
            TimeWindow::Week.start_in(now, "Africa/Nairobi"),
            Utc.with_ymd_and_hms(2026, 11, 8, 21, 0, 0).unwrap()
        );
        assert_eq!(
            TimeWindow::Day.start_in(now, "Not/AZone"),
            Utc.with_ymd_and_hms(2026, 11, 14, 0, 0, 0).unwrap()
        );
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
    /// "telegram:12345", "web", ... (who to act as when syncing).
    pub recorded_by_id: Option<String>,
    pub recorded_by_role: Option<Role>,
    pub voided: Option<Voided>,
    /// Why this entry isn't in Friday (yet), if it isn't.
    pub sync_error: Option<String>,
}

/// An entry to send to Friday: record it, or reverse it (voided after it
/// was sent).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncItem {
    pub entry: JournalEntry,
    pub reverse: bool,
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

    /// Sales, expenses and stock updates not yet in Friday, and voided ones
    /// whose reversal isn't, oldest first.
    async fn to_sync(&self, business_id: Uuid, limit: i64) -> Result<Vec<SyncItem>, StoreError>;
    async fn mark_synced(&self, id: Uuid, reverse: bool) -> Result<(), StoreError>;
    /// `permanent`: never retried (shown in /history); otherwise kept for
    /// the next attempt.
    async fn mark_sync_failed(
        &self,
        id: Uuid,
        error: &str,
        permanent: bool,
    ) -> Result<(), StoreError>;

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
    /// Counting entries from `since` on, oldest first.
    async fn list_events_since(
        &self,
        business_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<Vec<BusinessEvent>, StoreError>;
    /// Counting entries in a rolling window (the last 24 h, ...).
    async fn list_events(
        &self,
        business_id: Uuid,
        window: TimeWindow,
    ) -> Result<Vec<BusinessEvent>, StoreError> {
        self.list_events_since(business_id, window.start(Utc::now()))
            .await
    }
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
