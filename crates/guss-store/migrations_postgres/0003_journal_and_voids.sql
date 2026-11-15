-- Nothing is deleted: who recorded each entry, and voids instead of
-- deletes. Voided entries stop counting (totals, stock) but stay in the
-- journal with who voided them and why. Entries are numbered by seq.
ALTER TABLE events ADD COLUMN seq BIGSERIAL;
ALTER TABLE events ADD COLUMN recorded_by_id TEXT;
ALTER TABLE events ADD COLUMN recorded_by_name TEXT;
ALTER TABLE events ADD COLUMN recorded_by_role TEXT;
ALTER TABLE events ADD COLUMN voided_at TEXT;
ALTER TABLE events ADD COLUMN voided_by TEXT;
ALTER TABLE events ADD COLUMN void_reason TEXT;
CREATE UNIQUE INDEX idx_events_seq ON events (seq);

-- Asking for an entry to stop counting, decided by someone above.
CREATE TABLE void_requests (
    number BIGSERIAL PRIMARY KEY,
    business_id TEXT NOT NULL,
    entry BIGINT NOT NULL,
    requested_by_id TEXT NOT NULL,
    requested_by_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    requested_at TEXT NOT NULL,
    decided_by TEXT,
    approved BOOLEAN,
    decided_at TEXT
);
CREATE INDEX idx_void_requests_pending ON void_requests (business_id, decided_at);
