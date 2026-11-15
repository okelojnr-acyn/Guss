-- Syncing entries to Friday (Inventory first). synced_at: when the entry
-- reached Friday, or 'skipped' when it never will (no access, not linked,
-- a quantity Friday can't hold); sync_error says why, or holds the last
-- transient error while retrying. void_synced_at: when a voided entry's
-- reversal reached Friday.
ALTER TABLE events ADD COLUMN synced_at TEXT;
ALTER TABLE events ADD COLUMN sync_error TEXT;
ALTER TABLE events ADD COLUMN void_synced_at TEXT;
