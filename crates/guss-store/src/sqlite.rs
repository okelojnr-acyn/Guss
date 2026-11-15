use async_trait::async_trait;
use chrono::{DateTime, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile, Document, Item};
use guss_core::people::{Actor, Role};
use guss_core::store::{
    JournalEntry, StockLevel, Store, StoreError, TimeWindow, VoidRequest, Voided,
};
use sqlx::Row;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use uuid::Uuid;

use crate::event_columns::columns_for;

pub struct SqliteStore {
    pool: SqlitePool,
}

fn db_err(e: sqlx::Error) -> StoreError {
    StoreError::Database(e.to_string())
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StoreError::Database(format!("bad timestamp {s}: {e}")))
}

fn journal_from_row(row: sqlx::sqlite::SqliteRow) -> Result<JournalEntry, StoreError> {
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
        recorded_by_role: row
            .get::<Option<String>, _>("recorded_by_role")
            .as_deref()
            .and_then(Role::parse),
        voided,
    })
}

fn void_request_from_row(row: sqlx::sqlite::SqliteRow) -> Result<VoidRequest, StoreError> {
    Ok(VoidRequest {
        number: row.get("number"),
        entry: row.get("entry"),
        requested_by_id: row.get("requested_by_id"),
        requested_by: row.get("requested_by_name"),
        reason: row.get("reason"),
        requested_at: parse_time(&row.get::<String, _>("requested_at"))?,
    })
}

fn item_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Item, StoreError> {
    let corrupt = |e: serde_json::Error| StoreError::Database(format!("corrupt item: {e}"));
    Ok(Item {
        name: row.get("name"),
        base_unit: row.get("base_unit"),
        units: serde_json::from_str(&row.get::<String, _>("units")).map_err(corrupt)?,
        aliases: serde_json::from_str(&row.get::<String, _>("aliases")).map_err(corrupt)?,
    })
}

impl SqliteStore {
    /// `url` is a sqlx sqlite connection string, e.g. `sqlite://guss.db` or
    /// `sqlite::memory:` for tests. The database file is created if it
    /// doesn't exist yet (sqlx's default is to fail), so a fresh, empty
    /// data volume works on first start.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(db_err)?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(db_err)?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| StoreError::Database(e.to_string()))?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub async fn connect_in_memory() -> Result<Self, StoreError> {
        Self::connect("sqlite::memory:").await
    }
}

#[cfg(test)]
mod create_tests {
    use super::*;

    #[tokio::test]
    async fn creates_missing_database_file() {
        let dir = std::env::temp_dir().join(format!("guss-create-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("guss.db");
        let url = format!("sqlite://{}", path.display());
        SqliteStore::connect(&url)
            .await
            .expect("fresh database should be created");
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[async_trait]
impl Store for SqliteStore {
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            "UPDATE events SET voided_at = ?, voided_by = ?, void_reason = ? WHERE id = ? AND voided_at IS NULL",
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
            "SELECT rowid AS number, payload, recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason FROM events WHERE business_id = ? ORDER BY rowid DESC LIMIT ?",
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
            "SELECT rowid AS number, payload, recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason FROM events WHERE business_id = ? AND rowid = ?",
        )
        .bind(business_id.to_string())
        .bind(number)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        row.map(journal_from_row).transpose()
    }

    async fn request_void(
        &self,
        business_id: Uuid,
        entry: i64,
        by: &Actor,
        reason: &str,
    ) -> Result<i64, StoreError> {
        let row = sqlx::query(
            "INSERT INTO void_requests (business_id, entry, requested_by_id, requested_by_name, reason, requested_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING number",
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
            "SELECT number, entry, requested_by_id, requested_by_name, reason, requested_at FROM void_requests WHERE business_id = ? AND decided_at IS NULL ORDER BY number",
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
            "UPDATE void_requests SET decided_by = ?, approved = ?, decided_at = ? WHERE business_id = ? AND number = ? AND decided_at IS NULL RETURNING number, entry, requested_by_id, requested_by_name, reason, requested_at",
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

    async fn list_events(
        &self,
        business_id: Uuid,
        window: TimeWindow,
    ) -> Result<Vec<BusinessEvent>, StoreError> {
        let start = window.start(Utc::now()).to_rfc3339();
        let rows = sqlx::query(
            "SELECT payload FROM events WHERE business_id = ? AND occurred_at >= ? AND voided_at IS NULL ORDER BY occurred_at ASC",
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
                COALESCE((SELECT SUM(delta * unit_factor) FROM events WHERE business_id = ? AND kind = 'stock_update' AND item_name = ? AND voided_at IS NULL), 0.0)
                -
                COALESCE((SELECT SUM(quantity * unit_factor) FROM events WHERE business_id = ? AND kind = 'sale' AND item_name = ? AND voided_at IS NULL), 0.0)
                AS stock
            "#,
        )
        .bind(business_id.to_string())
        .bind(item_name)
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
                WHERE business_id = ? AND kind = 'stock_update' AND reorder_threshold IS NOT NULL AND voided_at IS NULL
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
                WHERE business_id = ? AND item_name IS NOT NULL AND voided_at IS NULL
                GROUP BY item_name
            )
            SELECT stock.item_name AS item_name, stock.current_quantity AS current_quantity, latest_threshold.reorder_threshold AS reorder_threshold
            FROM stock
            JOIN latest_threshold ON latest_threshold.item_name = stock.item_name
            WHERE stock.current_quantity <= latest_threshold.reorder_threshold
            "#,
        )
        .bind(business_id.to_string())
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
        sqlx::query("INSERT INTO documents (id, business_id, title, content, created_at) VALUES (?, ?, ?, ?, ?)")
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
        // FTS5 MATCH query: OR-join sanitized terms, e.g. "rice OR soap".
        let match_query = terms
            .iter()
            .map(|t| t.replace('"', ""))
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        if match_query.is_empty() {
            return Ok(vec![]);
        }

        let rows = sqlx::query(
            r#"
            SELECT d.id, d.business_id, d.title, d.content, d.created_at
            FROM documents_fts f
            JOIN documents d ON d.rowid = f.rowid
            WHERE f.content MATCH ? AND d.business_id = ?
            ORDER BY rank
            LIMIT ?
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
            VALUES (?, ?, ?, ?, ?)
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
            "SELECT name, base_unit, units, aliases FROM items WHERE business_id = ? ORDER BY name",
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
            VALUES (?, ?, ?, ?, ?)
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
            "INSERT INTO processed_messages (source, provider_message_id, processed_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
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

#[cfg(test)]
mod tests {
    use super::*;
    use guss_core::domain::{BusinessEvent, Sale, SourceChannel, StockUpdate};
    use guss_core::money::CurrencyAmount;

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
    async fn stock_is_counted_in_base_units() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
        let business_id = Uuid::new_v4();
        // 2 crates of 24 in, 5 bottles out.
        let mut crates = restock(business_id, "soda", 2.0, None);
        if let BusinessEvent::StockUpdate(su) = &mut crates {
            su.unit = Some("crate".into());
            su.unit_factor = 24.0;
        }
        store.save_event(&crates).await.unwrap();
        store
            .save_event(&sale(business_id, "soda", 5.0, 80.0))
            .await
            .unwrap();
        assert_eq!(
            store.current_stock(business_id, "soda").await.unwrap(),
            43.0
        );
    }

    #[tokio::test]
    async fn items_round_trip_and_update_in_place() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
        let business_id = Uuid::new_v4();
        let mut soda = Item {
            name: "soda".into(),
            base_unit: Some("bottle".into()),
            units: vec![("crate".into(), 24.0)],
            aliases: vec!["coke".into()],
        };
        store.save_item(business_id, &soda).await.unwrap();
        soda.units.push(("six-pack".into(), 6.0));
        store.save_item(business_id, &soda).await.unwrap();
        assert_eq!(store.list_items(business_id).await.unwrap(), vec![soda]);
        assert!(store.list_items(Uuid::new_v4()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn current_stock_is_restock_minus_sales() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
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
    async fn low_stock_items_flags_items_at_or_below_threshold() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
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
    async fn list_events_round_trips_through_payload_json() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
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
    async fn document_search_finds_matching_keyword() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
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
    async fn void_stops_it_counting_but_keeps_it_in_the_journal() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
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
    async fn void_requests_wait_until_decided_once() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
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
    async fn mark_message_processed_detects_redelivery() {
        let store = SqliteStore::connect_in_memory().await.unwrap();
        let first = store
            .mark_message_processed("telegram", "msg-1")
            .await
            .unwrap();
        let second = store
            .mark_message_processed("telegram", "msg-1")
            .await
            .unwrap();
        assert!(first, "first delivery should be newly recorded");
        assert!(!second, "redelivery should be detected as already seen");
    }
}
