use async_trait::async_trait;
use chrono::{DateTime, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile, Document, Item};
use guss_core::people::{Actor, Role};
use guss_core::store::{
    JournalEntry, StockLevel, Store, StoreError, SyncItem, TimeWindow, VoidRequest, Voided,
};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

use crate::event_columns::columns_for;

pub struct PostgresStore {
    pool: PgPool,
}

fn db_err(e: sqlx::Error) -> StoreError {
    StoreError::Database(e.to_string())
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StoreError::Database(format!("bad timestamp {s}: {e}")))
}

fn journal_from_row(row: sqlx::postgres::PgRow) -> Result<JournalEntry, StoreError> {
    let payload: String = row.get("payload");
    let event = serde_json::from_str::<BusinessEvent>(&payload)
        .map_err(|e| StoreError::Database(format!("corrupt event payload: {e}")))?;
    let voided = match row.get::<Option<String>, _>("voided_at") {
        Some(at) => Some(Voided {
            at: parse_time(&at)?,
            by: row
                .get::<Option<String>, _>("voided_by")
                .unwrap_or_default(),
            reason: row
                .get::<Option<String>, _>("void_reason")
                .unwrap_or_default(),
        }),
        None => None,
    };
    Ok(JournalEntry {
        number: row.get("number"),
        event,
        recorded_by: row.get("recorded_by_name"),
        recorded_by_id: row.get("recorded_by_id"),
        sync_error: row.get("sync_error"),
        recorded_by_role: row
            .get::<Option<String>, _>("recorded_by_role")
            .as_deref()
            .and_then(Role::parse),
        voided,
    })
}

fn void_request_from_row(row: sqlx::postgres::PgRow) -> Result<VoidRequest, StoreError> {
    Ok(VoidRequest {
        number: row.get("number"),
        entry: row.get("entry"),
        requested_by_id: row.get("requested_by_id"),
        requested_by: row.get("requested_by_name"),
        reason: row.get("reason"),
        requested_at: parse_time(&row.get::<String, _>("requested_at"))?,
    })
}

fn item_from_row(row: sqlx::postgres::PgRow) -> Result<Item, StoreError> {
    let corrupt = |e: serde_json::Error| StoreError::Database(format!("corrupt item: {e}"));
    Ok(Item {
        name: row.get("name"),
        base_unit: row.get("base_unit"),
        units: serde_json::from_str(&row.get::<String, _>("units")).map_err(corrupt)?,
        aliases: serde_json::from_str(&row.get::<String, _>("aliases")).map_err(corrupt)?,
    })
}

impl PostgresStore {
    /// `url` is a sqlx postgres connection string, e.g.
    /// `postgres://user:pass@host:5432/dbname`.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(db_err)?;
        sqlx::migrate!("./migrations_postgres")
            .run(&pool)
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Store for PostgresStore {
    async fn save_event_by(
        &self,
        event: &BusinessEvent,
        author: Option<&Actor>,
    ) -> Result<(), StoreError> {
        let c = columns_for(event);
        sqlx::query(
            r#"
            INSERT INTO events (
                id, business_id, kind, occurred_at, item_name, quantity, delta, unit, unit_factor,
                amount_minor, currency, category, description, reorder_threshold,
                recurring_interval, recurring_label, source_channel, raw_text, payload,
                recorded_by_id, recorded_by_name, recorded_by_role
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)
            "#,
        )
        .bind(c.id)
        .bind(c.business_id)
        .bind(c.kind)
        .bind(c.occurred_at)
        .bind(c.item_name)
        .bind(c.quantity)
        .bind(c.delta)
        .bind(c.unit)
        .bind(c.unit_factor)
        .bind(c.amount_minor)
        .bind(c.currency)
        .bind(c.category)
        .bind(c.description)
        .bind(c.reorder_threshold)
        .bind(c.recurring_interval)
        .bind(c.recurring_label)
        .bind(c.source_channel)
        .bind(c.raw_text)
        .bind(c.payload)
        .bind(author.map(|a| a.id.clone()))
        .bind(author.map(|a| a.name.clone()))
        .bind(author.map(|a| a.role.as_str()))
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn void_event(&self, id: Uuid, by: &Actor, reason: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE events SET voided_at = $1, voided_by = $2, void_reason = $3 WHERE id = $4 AND voided_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(&by.name)
        .bind(reason)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn journal(
        &self,
        business_id: Uuid,
        limit: i64,
    ) -> Result<Vec<JournalEntry>, StoreError> {
        let rows = sqlx::query(
            "SELECT seq AS number, payload, recorded_by_id, recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason, sync_error FROM events WHERE business_id = $1 ORDER BY seq DESC LIMIT $2",
        )
        .bind(business_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.into_iter().map(journal_from_row).collect()
    }

    async fn journal_entry(
        &self,
        business_id: Uuid,
        number: i64,
    ) -> Result<Option<JournalEntry>, StoreError> {
        let row = sqlx::query(
            "SELECT seq AS number, payload, recorded_by_id, recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason, sync_error FROM events WHERE business_id = $1 AND seq = $2",
        )
        .bind(business_id.to_string())
        .bind(number)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.map(journal_from_row).transpose()
    }

    async fn to_sync(&self, business_id: Uuid, limit: i64) -> Result<Vec<SyncItem>, StoreError> {
        let rows = sqlx::query(
            "SELECT seq AS number, payload, recorded_by_id, recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason, sync_error, (voided_at IS NOT NULL) AS reverse FROM events WHERE business_id = $1 AND kind IN ('sale', 'stock_update') AND ((synced_at IS NULL AND voided_at IS NULL) OR (voided_at IS NOT NULL AND synced_at IS NOT NULL AND synced_at <> 'skipped' AND void_synced_at IS NULL)) ORDER BY seq LIMIT $2",
        )
        .bind(business_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.into_iter()
            .map(|row| {
                let reverse: bool = row.get("reverse");
                Ok(SyncItem {
                    entry: journal_from_row(row)?,
                    reverse,
                })
            })
            .collect()
    }

    async fn mark_synced(&self, id: Uuid, reverse: bool) -> Result<(), StoreError> {
        let sql = if reverse {
            "UPDATE events SET void_synced_at = $1, sync_error = NULL WHERE id = $2"
        } else {
            "UPDATE events SET synced_at = $1, sync_error = NULL WHERE id = $2"
        };
        sqlx::query(sql)
            .bind(Utc::now().to_rfc3339())
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn mark_sync_failed(
        &self,
        id: Uuid,
        error: &str,
        permanent: bool,
    ) -> Result<(), StoreError> {
        let sql = if permanent {
            "UPDATE events SET sync_error = $1, synced_at = COALESCE(synced_at, 'skipped'), void_synced_at = CASE WHEN synced_at IS NULL THEN void_synced_at ELSE COALESCE(void_synced_at, 'skipped') END WHERE id = $2"
        } else {
            "UPDATE events SET sync_error = $1 WHERE id = $2"
        };
        sqlx::query(sql)
            .bind(error)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn request_void(
        &self,
        business_id: Uuid,
        entry: i64,
        by: &Actor,
        reason: &str,
    ) -> Result<i64, StoreError> {
        let row = sqlx::query(
            "INSERT INTO void_requests (business_id, entry, requested_by_id, requested_by_name, reason, requested_at) VALUES ($1, $2, $3, $4, $5, $6) RETURNING number",
        )
        .bind(business_id.to_string())
        .bind(entry)
        .bind(&by.id)
        .bind(&by.name)
        .bind(reason)
        .bind(Utc::now().to_rfc3339())
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.get::<i64, _>("number"))
    }

    async fn pending_void_requests(
        &self,
        business_id: Uuid,
    ) -> Result<Vec<VoidRequest>, StoreError> {
        let rows = sqlx::query(
            "SELECT number, entry, requested_by_id, requested_by_name, reason, requested_at FROM void_requests WHERE business_id = $1 AND decided_at IS NULL ORDER BY number",
        )
        .bind(business_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.into_iter().map(void_request_from_row).collect()
    }

    async fn decide_void_request(
        &self,
        business_id: Uuid,
        number: i64,
        by: &Actor,
        approved: bool,
    ) -> Result<Option<VoidRequest>, StoreError> {
        let row = sqlx::query(
            "UPDATE void_requests SET decided_by = $1, approved = $2, decided_at = $3 WHERE business_id = $4 AND number = $5 AND decided_at IS NULL RETURNING number, entry, requested_by_id, requested_by_name, reason, requested_at",
        )
        .bind(&by.name)
        .bind(approved)
        .bind(Utc::now().to_rfc3339())
        .bind(business_id.to_string())
        .bind(number)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.map(void_request_from_row).transpose()
    }

    async fn list_events_since(
        &self,
        business_id: Uuid,
        since: DateTime<Utc>,
    ) -> Result<Vec<BusinessEvent>, StoreError> {
        let start = since.to_rfc3339();
        let rows = sqlx::query(
            "SELECT payload FROM events WHERE business_id = $1 AND occurred_at >= $2 AND voided_at IS NULL ORDER BY occurred_at ASC",
        )
        .bind(business_id.to_string())
        .bind(start)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.into_iter()
            .map(|row| {
                let payload: String = row.get("payload");
                serde_json::from_str::<BusinessEvent>(&payload)
                    .map_err(|e| StoreError::Database(format!("corrupt event payload: {e}")))
            })
            .collect()
    }

    async fn current_stock(&self, business_id: Uuid, item_name: &str) -> Result<f64, StoreError> {
        let row = sqlx::query(
            r#"
            SELECT
                COALESCE((SELECT SUM(delta * unit_factor) FROM events WHERE business_id = $1 AND kind = 'stock_update' AND item_name = $2 AND voided_at IS NULL), 0.0)
                -
                COALESCE((SELECT SUM(quantity * unit_factor) FROM events WHERE business_id = $1 AND kind = 'sale' AND item_name = $2 AND voided_at IS NULL), 0.0)
                AS stock
            "#,
        )
        .bind(business_id.to_string())
        .bind(item_name)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.get::<f64, _>("stock"))
    }

    async fn low_stock_items(&self, business_id: Uuid) -> Result<Vec<StockLevel>, StoreError> {
        // Items that have ever had a reorder threshold set, paired with
        // their most recent threshold value and current derived stock.
        let rows = sqlx::query(
            r#"
            WITH latest_threshold AS (
                SELECT item_name, reorder_threshold
                FROM events
                WHERE business_id = $1 AND kind = 'stock_update' AND reorder_threshold IS NOT NULL AND voided_at IS NULL
                AND occurred_at = (
                    SELECT MAX(occurred_at) FROM events e2
                    WHERE e2.business_id = events.business_id AND e2.item_name = events.item_name
                    AND e2.kind = 'stock_update' AND e2.reorder_threshold IS NOT NULL AND e2.voided_at IS NULL
                )
            ),
            stock AS (
                SELECT item_name,
                    COALESCE(SUM(CASE WHEN kind = 'stock_update' THEN delta * unit_factor ELSE 0 END), 0.0)
                    - COALESCE(SUM(CASE WHEN kind = 'sale' THEN quantity * unit_factor ELSE 0 END), 0.0) AS current_quantity
                FROM events
                WHERE business_id = $1 AND item_name IS NOT NULL AND voided_at IS NULL
                GROUP BY item_name
            )
            SELECT stock.item_name AS item_name, stock.current_quantity AS current_quantity, latest_threshold.reorder_threshold AS reorder_threshold
            FROM stock
            JOIN latest_threshold ON latest_threshold.item_name = stock.item_name
            WHERE stock.current_quantity <= latest_threshold.reorder_threshold
            "#,
        )
        .bind(business_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(rows
            .into_iter()
            .map(|row| StockLevel {
                item_name: row.get("item_name"),
                current_quantity: row.get("current_quantity"),
                reorder_threshold: row.get("reorder_threshold"),
            })
            .collect())
    }

    async fn save_document(&self, doc: &Document) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO documents (id, business_id, title, content, created_at) VALUES ($1, $2, $3, $4, $5)")
            .bind(doc.id.to_string())
            .bind(doc.business_id.to_string())
            .bind(&doc.title)
            .bind(&doc.content)
            .bind(doc.created_at.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn search_documents(
        &self,
        business_id: Uuid,
        terms: &[String],
        limit: i64,
    ) -> Result<Vec<Document>, StoreError> {
        if terms.is_empty() {
            return Ok(vec![]);
        }
        // websearch_to_tsquery natively understands "a or b" - this mirrors
        // the SQLite FTS5 side's "\"a\" OR \"b\"" OR-of-terms behavior.
        let match_query = terms
            .iter()
            .map(|t| t.replace('"', ""))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" or ");
        if match_query.is_empty() {
            return Ok(vec![]);
        }

        let rows = sqlx::query(
            r#"
            SELECT id, business_id, title, content, created_at
            FROM documents
            WHERE content_tsv @@ websearch_to_tsquery('english', $1) AND business_id = $2
            ORDER BY ts_rank(content_tsv, websearch_to_tsquery('english', $1)) DESC
            LIMIT $3
            "#,
        )
        .bind(match_query)
        .bind(business_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.get("id");
                let business_id: String = row.get("business_id");
                let created_at: String = row.get("created_at");
                Ok(Document {
                    id: Uuid::parse_str(&id).map_err(|e| StoreError::Database(e.to_string()))?,
                    business_id: Uuid::parse_str(&business_id)
                        .map_err(|e| StoreError::Database(e.to_string()))?,
                    title: row.get("title"),
                    content: row.get("content"),
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .map_err(|e| StoreError::Database(e.to_string()))?
                        .with_timezone(&Utc),
                })
            })
            .collect()
    }

    async fn save_item(&self, business_id: Uuid, item: &Item) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            INSERT INTO items (business_id, name, base_unit, units, aliases)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(business_id, name) DO UPDATE SET
                base_unit = excluded.base_unit,
                units = excluded.units,
                aliases = excluded.aliases
            "#,
        )
        .bind(business_id.to_string())
        .bind(&item.name)
        .bind(&item.base_unit)
        .bind(serde_json::to_string(&item.units).expect("units serialize"))
        .bind(serde_json::to_string(&item.aliases).expect("aliases serialize"))
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn list_items(&self, business_id: Uuid) -> Result<Vec<Item>, StoreError> {
        let rows = sqlx::query(
            "SELECT name, base_unit, units, aliases FROM items WHERE business_id = $1 ORDER BY name",
        )
        .bind(business_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.into_iter().map(item_from_row).collect()
    }

    async fn save_business_profile(&self, profile: &BusinessProfile) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            INSERT INTO business_profile (id, name, default_currency, language, timezone)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                default_currency = excluded.default_currency,
                language = excluded.language,
                timezone = excluded.timezone
            "#,
        )
        .bind(profile.id.to_string())
        .bind(&profile.name)
        .bind(&profile.default_currency)
        .bind(&profile.language)
        .bind(&profile.timezone)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_business_profile(&self) -> Result<Option<BusinessProfile>, StoreError> {
        let row = sqlx::query(
            "SELECT id, name, default_currency, language, timezone FROM business_profile LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        match row {
            None => Ok(None),
            Some(row) => {
                let id: String = row.get("id");
                Ok(Some(BusinessProfile {
                    id: Uuid::parse_str(&id).map_err(|e| StoreError::Database(e.to_string()))?,
                    name: row.get("name"),
                    default_currency: row.get("default_currency"),
                    language: row.get("language"),
                    timezone: row.get("timezone"),
                }))
            }
        }
    }

    async fn mark_message_processed(
        &self,
        source: &str,
        provider_message_id: &str,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "INSERT INTO processed_messages (source, provider_message_id, processed_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(source)
        .bind(provider_message_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(result.rows_affected() == 1)
    }
}

/// These tests need a real Postgres instance (`GUSS_TEST_POSTGRES_URL`),
/// so they're `#[ignore]`d by default - they run in CI's
/// `postgres-integration` job (see .github/workflows/ci.yml) against a
/// `postgres:16-alpine` service container, or locally if you point the
/// env var at your own instance. They intentionally mirror sqlite.rs's
/// test module so the two backends are checked against the same
/// behavioral assertions.
#[cfg(test)]
mod tests {
    use super::*;
    use guss_core::domain::{BusinessEvent, Sale, SourceChannel, StockUpdate};
    use guss_core::money::CurrencyAmount;

    async fn test_store() -> PostgresStore {
        let url = std::env::var("GUSS_TEST_POSTGRES_URL").expect(
            "GUSS_TEST_POSTGRES_URL must be set to run PostgresStore tests - \
             see CONTRIBUTING.md for a local Postgres one-liner, or run these in CI.",
        );
        PostgresStore::connect(&url).await.unwrap()
    }

    fn sale(business_id: Uuid, item: &str, qty: f64, price: f64) -> BusinessEvent {
        BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id,
            item_name: item.into(),
            quantity: qty,
            unit: None,
            unit_factor: 1.0,
            unit_price: CurrencyAmount::new(price, "USD"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            raw_text: Some(format!("sold {qty} {item} at {price} each")),
        })
    }

    fn restock(business_id: Uuid, item: &str, delta: f64, threshold: Option<f64>) -> BusinessEvent {
        BusinessEvent::StockUpdate(StockUpdate {
            id: Uuid::new_v4(),
            business_id,
            item_name: item.into(),
            delta,
            unit: Some("bags".into()),
            unit_factor: 1.0,
            unit_cost: None,
            reorder_threshold: threshold,
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Cli,
            raw_text: None,
        })
    }

    #[tokio::test]
    #[ignore]
    async fn current_stock_is_restock_minus_sales() {
        let store = test_store().await;
        let business_id = Uuid::new_v4();

        store
            .save_event(&restock(business_id, "rice", 20.0, Some(5.0)))
            .await
            .unwrap();
        store
            .save_event(&sale(business_id, "rice", 3.0, 500.0))
            .await
            .unwrap();
        store
            .save_event(&sale(business_id, "rice", 2.0, 500.0))
            .await
            .unwrap();

        let stock = store.current_stock(business_id, "rice").await.unwrap();
        assert_eq!(stock, 15.0);
    }

    #[tokio::test]
    #[ignore]
    async fn low_stock_items_flags_items_at_or_below_threshold() {
        let store = test_store().await;
        let business_id = Uuid::new_v4();

        store
            .save_event(&restock(business_id, "rice", 10.0, Some(5.0)))
            .await
            .unwrap();
        store
            .save_event(&sale(business_id, "rice", 6.0, 500.0))
            .await
            .unwrap();
        store
            .save_event(&restock(business_id, "soap", 50.0, Some(5.0)))
            .await
            .unwrap();

        let low = store.low_stock_items(business_id).await.unwrap();
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].item_name, "rice");
        assert_eq!(low[0].current_quantity, 4.0);
    }

    #[tokio::test]
    #[ignore]
    async fn list_events_round_trips_through_payload_json() {
        let store = test_store().await;
        let business_id = Uuid::new_v4();
        let event = sale(business_id, "soap", 1.0, 1200.0);

        store.save_event(&event).await.unwrap();
        let events = store
            .list_events(business_id, TimeWindow::Day)
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], event);
    }

    #[tokio::test]
    #[ignore]
    async fn document_search_finds_matching_keyword() {
        let store = test_store().await;
        let business_id = Uuid::new_v4();
        store
            .save_document(&guss_core::domain::Document {
                id: Uuid::new_v4(),
                business_id,
                title: "Price list".into(),
                content: "rice is 500 per bag, soap is 1200 per unit".into(),
                created_at: Utc::now(),
            })
            .await
            .unwrap();

        let found = store
            .search_documents(business_id, &["rice".to_string()], 3)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].content.contains("rice"));
    }

    #[tokio::test]
    #[ignore]
    async fn void_stops_it_counting_but_keeps_it_in_the_journal() {
        let store = test_store().await;
        let business_id = Uuid::new_v4();
        let event = restock(business_id, "rice", 10.0, None);
        let event_id = event.id();
        let mary = Actor::new("telegram:1", "Mary", Role::Staff);
        let boss = Actor::new("telegram:2", "Boss", Role::Owner);

        store.save_event_by(&event, Some(&mary)).await.unwrap();
        assert_eq!(
            store.current_stock(business_id, "rice").await.unwrap(),
            10.0
        );

        store
            .void_event(event_id, &boss, "counted twice")
            .await
            .unwrap();
        assert_eq!(store.current_stock(business_id, "rice").await.unwrap(), 0.0);
        assert!(
            store
                .list_events(business_id, TimeWindow::Day)
                .await
                .unwrap()
                .is_empty()
        );

        let journal = store.journal(business_id, 10).await.unwrap();
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].event, event);
        assert_eq!(journal[0].recorded_by.as_deref(), Some("Mary"));
        assert_eq!(journal[0].recorded_by_role, Some(Role::Staff));
        let voided = journal[0].voided.as_ref().unwrap();
        assert_eq!(
            (voided.by.as_str(), voided.reason.as_str()),
            ("Boss", "counted twice")
        );
        let same = store
            .journal_entry(business_id, journal[0].number)
            .await
            .unwrap();
        assert_eq!(same, Some(journal[0].clone()));

        // Voiding again changes nothing.
        store.void_event(event_id, &mary, "again").await.unwrap();
        let journal = store.journal(business_id, 10).await.unwrap();
        assert_eq!(journal[0].voided.as_ref().unwrap().by, "Boss");
    }

    #[tokio::test]
    #[ignore]
    async fn void_requests_wait_until_decided_once() {
        let store = test_store().await;
        let business_id = Uuid::new_v4();
        let mary = Actor::new("telegram:1", "Mary", Role::Staff);
        let boss = Actor::new("telegram:2", "Boss", Role::Owner);
        let first = store
            .request_void(business_id, 7, &mary, "typo")
            .await
            .unwrap();
        let second = store
            .request_void(business_id, 8, &mary, "twice")
            .await
            .unwrap();
        assert!(second > first);
        let pending = store.pending_void_requests(business_id).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            (
                pending[0].entry,
                pending[0].requested_by.as_str(),
                pending[0].reason.as_str()
            ),
            (7, "Mary", "typo")
        );
        assert_eq!(pending[0].requested_by_id, "telegram:1");

        let decided = store
            .decide_void_request(business_id, first, &boss, true)
            .await
            .unwrap();
        assert_eq!(decided.map(|r| r.entry), Some(7));
        // Decided requests can't be decided again.
        assert_eq!(
            store
                .decide_void_request(business_id, first, &boss, false)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .pending_void_requests(business_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .pending_void_requests(Uuid::new_v4())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    #[ignore]
    async fn mark_message_processed_detects_redelivery() {
        let store = test_store().await;
        let source = format!("test-{}", Uuid::new_v4());
        let first = store
            .mark_message_processed(&source, "msg-1")
            .await
            .unwrap();
        let second = store
            .mark_message_processed(&source, "msg-1")
            .await
            .unwrap();
        assert!(first, "first delivery should be newly recorded");
        assert!(!second, "redelivery should be detected as already seen");
    }
}
