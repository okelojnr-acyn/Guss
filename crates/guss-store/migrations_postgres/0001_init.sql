-- Postgres counterpart of migrations/0001_init.sql. Column names/types
-- deliberately mirror the SQLite schema (ids/timestamps stay TEXT rather
-- than native UUID/TIMESTAMPTZ) so the two backends behave identically
-- and share row-mapping logic in event_columns.rs - see
-- docs/ARCHITECTURE.md's Storage section.

CREATE TABLE business_profile (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    default_currency TEXT NOT NULL,
    language TEXT NOT NULL,
    timezone TEXT NOT NULL
);

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    business_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('sale', 'expense', 'stock_update', 'note')),
    occurred_at TEXT NOT NULL,
    item_name TEXT,
    quantity DOUBLE PRECISION,
    delta DOUBLE PRECISION,
    unit TEXT,
    amount_minor BIGINT,
    currency TEXT,
    category TEXT,
    description TEXT,
    reorder_threshold DOUBLE PRECISION,
    recurring_interval TEXT,
    recurring_label TEXT,
    source_channel TEXT NOT NULL,
    raw_text TEXT,
    payload TEXT NOT NULL
);

CREATE INDEX idx_events_business_window ON events (business_id, occurred_at);
CREATE INDEX idx_events_business_kind_item ON events (business_id, kind, item_name);

-- Reference documents (price lists, supplier notes) for NLU context.
-- Full-text search uses a generated tsvector column + GIN index instead
-- of SQLite's FTS5 virtual table/triggers - Postgres keeps it in sync
-- automatically, so no insert/update/delete triggers are needed here.
CREATE TABLE documents (
    id TEXT PRIMARY KEY,
    business_id TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    content_tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', content)) STORED
);

CREATE INDEX idx_documents_tsv ON documents USING GIN (content_tsv);

-- Idempotency guard against webhook redelivery (WhatsApp/Telegram both
-- redeliver on timeout) - see docs/ARCHITECTURE.md.
CREATE TABLE processed_messages (
    source TEXT NOT NULL,
    provider_message_id TEXT NOT NULL,
    processed_at TEXT NOT NULL,
    PRIMARY KEY (source, provider_message_id)
);
