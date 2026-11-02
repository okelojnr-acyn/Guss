use async_trait::async_trait;
use guss_core::domain::{BusinessEvent, BusinessProfile, Document};
use guss_core::store::{StockLevel, Store, StoreError, TimeWindow};
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
    async fn save_event(&self, event: &BusinessEvent) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.save_event(event).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.save_event(event).await,
        }
    }

    async fn delete_event(&self, id: Uuid) -> Result<(), StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.delete_event(id).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.delete_event(id).await,
        }
    }

    async fn list_events(
        &self,
        business_id: Uuid,
        window: TimeWindow,
    ) -> Result<Vec<BusinessEvent>, StoreError> {
        match self {
            AnyStore::Sqlite(s) => s.list_events(business_id, window).await,
            #[cfg(feature = "postgres")]
            AnyStore::Postgres(s) => s.list_events(business_id, window).await,
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
