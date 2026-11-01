-- Single-business profile for this Guss instance (Phase 1 is one business
-- per instance; business_id is still carried on every row for forward
-- compatibility with a future multi-tenant mode).
CREATE TABLE business_profile (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    default_currency TEXT NOT NULL,
    language TEXT NOT NULL,
    timezone TEXT NOT NULL
);

-- One row per extracted BusinessEvent. Frequently-queried fields are
-- pulled out into real columns so aggregation (guss-insights) doesn't need
-- to parse JSON in SQL; `payload` holds the full serialized event for a
-- lossless round trip back into the BusinessEvent enum.
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    business_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('sale', 'expense', 'stock_update', 'note')),
    occurred_at TEXT NOT NULL,
    item_name TEXT,
    quantity REAL,
    delta REAL,
    unit TEXT,
    amount_minor INTEGER,
    currency TEXT,
    category TEXT,
    description TEXT,
    reorder_threshold REAL,
    recurring_interval TEXT,
    recurring_label TEXT,
    source_channel TEXT NOT NULL,
    raw_text TEXT,
    payload TEXT NOT NULL
);

CREATE INDEX idx_events_business_window ON events (business_id, occurred_at);
CREATE INDEX idx_events_business_kind_item ON events (business_id, kind, item_name);

-- Reference documents (price lists, supplier notes) for NLU context.
CREATE TABLE documents (
    id TEXT PRIMARY KEY,
    business_id TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Lightweight RAG retrieval: SQLite FTS5 keyword search over document
-- content. If the SQLite build in use lacks FTS5, this migration fails
-- loudly at startup rather than silently losing document search - see
-- docs/ARCHITECTURE.md for the LIKE-based fallback path.
CREATE VIRTUAL TABLE documents_fts USING fts5(
    content,
    content = documents,
    content_rowid = rowid
);

CREATE TRIGGER documents_ai AFTER INSERT ON documents BEGIN
    INSERT INTO documents_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER documents_ad AFTER DELETE ON documents BEGIN
    INSERT INTO documents_fts(documents_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER documents_au AFTER UPDATE ON documents BEGIN
    INSERT INTO documents_fts(documents_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
    INSERT INTO documents_fts(rowid, content) VALUES (new.rowid, new.content);
END;

-- Idempotency guard against webhook redelivery (WhatsApp/Telegram both
-- redeliver on timeout) - see docs/ARCHITECTURE.md.
CREATE TABLE processed_messages (
    source TEXT NOT NULL,
    provider_message_id TEXT NOT NULL,
    processed_at TEXT NOT NULL,
    PRIMARY KEY (source, provider_message_id)
);
