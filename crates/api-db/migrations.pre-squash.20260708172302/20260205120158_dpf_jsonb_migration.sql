-- Add migration script here
-- Convert dpf_enabled (boolean) to dpf (jsonb) with structure {enabled: bool, used_for_ingestion: bool}

-- Step 1: Add the new dpf column as JSONB (nullable initially)
ALTER TABLE machines ADD COLUMN dpf JSONB;

-- Step 2: Migrate data from dpf_enabled to dpf.enabled, initialize used_for_ingestion to false and last_monitor_cycle_executed_at to current UTC time
UPDATE machines
SET dpf = jsonb_build_object(
    'enabled', dpf_enabled,
    'used_for_ingestion', false
);

-- Step 3: Make dpf NOT NULL now that all rows have values
ALTER TABLE machines ALTER COLUMN dpf SET NOT NULL;

-- Step 4: Drop the old dpf_enabled column
ALTER TABLE machines DROP COLUMN dpf_enabled;
