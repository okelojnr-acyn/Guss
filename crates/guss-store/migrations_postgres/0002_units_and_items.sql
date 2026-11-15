-- Units of measure. Events keep the quantity and unit as said ("2 crates")
-- plus how many base units one of that unit holds; stock is counted in
-- base units (quantity * unit_factor). Existing rows are factor 1.
ALTER TABLE events ADD COLUMN unit_factor DOUBLE PRECISION NOT NULL DEFAULT 1.0;

-- The shop's items: base unit, other units with their factor, aliases.
-- units/aliases are JSON arrays, read whole by the app.
CREATE TABLE items (
    business_id TEXT NOT NULL,
    name TEXT NOT NULL,
    base_unit TEXT,
    units TEXT NOT NULL DEFAULT '[]',
    aliases TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (business_id, name)
);
