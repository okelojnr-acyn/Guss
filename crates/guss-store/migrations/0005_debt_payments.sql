-- Customers paying for credit sales are a new kind of entry
-- ("debt_payment"). SQLite can't change a CHECK in place, so the table is
-- rebuilt with every row, keeping each rowid: rowids are the entry
-- numbers people see in /history and use in /void.
CREATE TABLE events_new (
    id TEXT PRIMARY KEY,
    business_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('sale', 'expense', 'stock_update', 'note', 'debt_payment')),
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
    payload TEXT NOT NULL,
    unit_factor REAL NOT NULL DEFAULT 1.0,
    recorded_by_id TEXT,
    recorded_by_name TEXT,
    recorded_by_role TEXT,
    voided_at TEXT,
    voided_by TEXT,
    void_reason TEXT,
    synced_at TEXT,
    sync_error TEXT,
    void_synced_at TEXT
);

INSERT INTO events_new (rowid, id, business_id, kind, occurred_at, item_name, quantity, delta, unit,
    amount_minor, currency, category, description, reorder_threshold, recurring_interval,
    recurring_label, source_channel, raw_text, payload, unit_factor, recorded_by_id,
    recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason, synced_at, sync_error,
    void_synced_at)
SELECT rowid, id, business_id, kind, occurred_at, item_name, quantity, delta, unit,
    amount_minor, currency, category, description, reorder_threshold, recurring_interval,
    recurring_label, source_channel, raw_text, payload, unit_factor, recorded_by_id,
    recorded_by_name, recorded_by_role, voided_at, voided_by, void_reason, synced_at, sync_error,
    void_synced_at
FROM events;

DROP TABLE events;
ALTER TABLE events_new RENAME TO events;
CREATE INDEX idx_events_business_window ON events (business_id, occurred_at);
CREATE INDEX idx_events_business_kind_item ON events (business_id, kind, item_name);
