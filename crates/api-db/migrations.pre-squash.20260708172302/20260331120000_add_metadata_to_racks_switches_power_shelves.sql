-- Add first-class metadata (name, description, labels) to realized rack, switch,
-- and power_shelf entities, matching the pattern already used by machines.

-- Racks (no pre-existing name column, so add name)
ALTER TABLE racks
    ADD COLUMN labels JSONB NOT NULL DEFAULT ('{}'),
    ADD COLUMN name VARCHAR(256) NOT NULL DEFAULT (''),
    ADD COLUMN description VARCHAR(1024) NOT NULL DEFAULT (''),
    ADD COLUMN version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268');
UPDATE racks SET name = id;

-- Switches: repurpose existing `name` column as metadata name (matching machines).
-- Serial number remains in the `config` JSONB (indexed by idx_switches_name).
ALTER TABLE switches DROP CONSTRAINT switches_name_key;
ALTER TABLE switches
    ADD COLUMN labels JSONB NOT NULL DEFAULT ('{}'),
    ADD COLUMN description VARCHAR(1024) NOT NULL DEFAULT (''),
    ADD COLUMN version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268');
UPDATE switches SET name = id;

-- Power shelves: same approach as switches.
ALTER TABLE power_shelves DROP CONSTRAINT power_shelves_name_key;
ALTER TABLE power_shelves
    ADD COLUMN labels JSONB NOT NULL DEFAULT ('{}'),
    ADD COLUMN description VARCHAR(1024) NOT NULL DEFAULT (''),
    ADD COLUMN version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268');
UPDATE power_shelves SET name = id;
