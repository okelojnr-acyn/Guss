use async_trait::async_trait;
use guss_core::domain::{BusinessEvent, BusinessProfile, Document, Item};
use guss_core::people::Actor;
use guss_core::store::{
    JournalEntry, StockLevel, Store, StoreError, SyncItem, TimeWindow, VoidRequest,
};
use uuid::Uuid;

#[cfg(feature = "postgres")]
use crate::PostgresStore;
use crate::SqliteStore;

/// Picks a `Store` implementation at runtime from a connection URL's
/// scheme, without turning every generic `S: Store` call site (channel
/// adapters, `Orchestrator`, the scheduler) into a trait object - they
/// stay generic over this one concrete type instead. See
/// docs/ARCHITECTURE.md's Storage section for why this shape was chosen
/// over `Arc<dyn Store>`.
pub enum AnyStore {
    Sqlite(SqliteStore),
    #[cfg(feature = "postgres")]
    Postgres(PostgresStore),
}

impl AnyStore {
    /// A consistent copy of the whole database, for backups. SQLite only:
    /// a Postgres store is backed up with the database itself.
    pub async fn snapshot(&self) -> Result<Vec<u8>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.snapshot().await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(_) => Err(StoreError::Database(
                "a Postgres store is backed up with its database, not by Guss".into(),
            )),
        }
    }

    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        if url.starts_with("sqlite:") {
            return Ok(AnyStore::Sqlite(SqliteStore::connect(url).await?));
        }

        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            #[cfg(feature = "postgres")]
            {
                return Ok(AnyStore::Postgres(
                    crate::PostgresStore::connect(url).await?,
                ));
            }
            #[cfg(not(feature = "postgres"))]
            {
                return Err(StoreError::Database(format!(
                    "GUSS_STORE__URL=\"{url}\" is a Postgres URL, but guss-store wasn't built with \
                     the \"postgres\" feature - rebuild with --features postgres to use it."
                )));
            }
        }

        Err(StoreError::Database(format!(
            "unrecognized store URL scheme in \"{url}\" - use a sqlite:// URL, or a postgres://\
             /postgresql:// URL built with --features postgres."
        )))
    }
}

#[async_trait]
impl Store for AnyStore {
    async fn save_event_by(
        &self,
        event: &BusinessEvent,
        author: Option<&Actor>,
    ) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.save_event_by(event, author).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.save_event_by(event, author).await,
        }
    }

    async fn void_event(&self, id: Uuid, by: &Actor, reason: &str) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.void_event(id, by, reason).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.void_event(id, by, reason).await,
        }
    }

    async fn journal(
        &self,
        business_id: Uuid,
        limit: i64,
    ) -> Result<Vec<JournalEntry>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.journal(business_id, limit).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.journal(business_id, limit).await,
        }
    }

    async fn journal_entry(
        &self,
        business_id: Uuid,
        number: i64,
    ) -> Result<Option<JournalEntry>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.journal_entry(business_id, number).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.journal_entry(business_id, number).await,
        }
    }

    async fn to_sync(&self, business_id: Uuid, limit: i64) -> Result<Vec<SyncItem>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.to_sync(business_id, limit).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.to_sync(business_id, limit).await,
        }
    }

    async fn mark_synced(&self, id: Uuid, reverse: bool) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.mark_synced(id, reverse).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.mark_synced(id, reverse).await,
        }
    }

    async fn mark_sync_failed(
        &self,
        id: Uuid,
        error: &str,
        permanent: bool,
    ) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.mark_sync_failed(id, error, permanent).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.mark_sync_failed(id, error, permanent).await,
        }
    }

    async fn request_void(
        &self,
        business_id: Uuid,
        entry: i64,
        by: &Actor,
        reason: &str,
    ) -> Result<i64, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.request_void(business_id, entry, by, reason).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.request_void(business_id, entry, by, reason).await,
        }
    }

    async fn pending_void_requests(
        &self,
        business_id: Uuid,
    ) -> Result<Vec<VoidRequest>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.pending_void_requests(business_id).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.pending_void_requests(business_id).await,
        }
    }

    async fn decide_void_request(
        &self,
        business_id: Uuid,
        number: i64,
        by: &Actor,
        approved: bool,
    ) -> Result<Option<VoidRequest>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => {
                s.decide_void_request(business_id, number, by, approved)
                    .await
            }
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => {
                s.decide_void_request(business_id, number, by, approved)
                    .await
            }
        }
    }

    async fn list_events_since(
        &self,
        business_id: Uuid,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<BusinessEvent>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.list_events_since(business_id, since).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.list_events_since(business_id, since).await,
        }
    }

    async fn current_stock(&self, business_id: Uuid, item_name: &str) -> Result<f64, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.current_stock(business_id, item_name).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.current_stock(business_id, item_name).await,
        }
    }

    async fn low_stock_items(&self, business_id: Uuid) -> Result<Vec<StockLevel>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.low_stock_items(business_id).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.low_stock_items(business_id).await,
        }
    }

    async fn save_document(&self, doc: &Document) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.save_document(doc).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.save_document(doc).await,
        }
    }

    async fn search_documents(
        &self,
        business_id: Uuid,
        terms: &[String],
        limit: i64,
    ) -> Result<Vec<Document>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.search_documents(business_id, terms, limit).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.search_documents(business_id, terms, limit).await,
        }
    }

    async fn save_item(&self, business_id: Uuid, item: &Item) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.save_item(business_id, item).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.save_item(business_id, item).await,
        }
    }

    async fn list_items(&self, business_id: Uuid) -> Result<Vec<Item>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.list_items(business_id).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.list_items(business_id).await,
        }
    }

    async fn save_business_profile(&self, profile: &BusinessProfile) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.save_business_profile(profile).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.save_business_profile(profile).await,
        }
    }

    async fn load_business_profile(&self) -> Result<Option<BusinessProfile>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.load_business_profile().await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.load_business_profile().await,
        }
    }

    async fn mark_message_processed(
        &self,
        source: &str,
        provider_message_id: &str,
    ) -> Result<bool, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.mark_message_processed(source, provider_message_id).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.mark_message_processed(source, provider_message_id).await,
        }
    }
}
