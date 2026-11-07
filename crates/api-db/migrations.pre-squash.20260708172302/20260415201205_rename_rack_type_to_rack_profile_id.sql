-- Add rack_profile_id as a proper column on the racks table.
ALTER TABLE racks ADD COLUMN rack_profile_id VARCHAR(256);

-- Backfill from the JSONB config column for existing racks (stored as rack_type).
UPDATE racks SET rack_profile_id = config->>'rack_type' WHERE config->>'rack_type' IS NOT NULL;

-- Rename the column on expected_racks too (was rack_type).
ALTER TABLE expected_racks RENAME COLUMN rack_type TO rack_profile_id;
