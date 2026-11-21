-- Customers paying for credit sales are a new kind of entry.
ALTER TABLE events DROP CONSTRAINT events_kind_check;
ALTER TABLE events ADD CONSTRAINT events_kind_check
    CHECK (kind IN ('sale', 'expense', 'stock_update', 'note', 'debt_payment'));
