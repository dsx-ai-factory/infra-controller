-- Change the default controller_state for new racks from "unknown" to "created"
-- to match the renamed RackControllerState enum (Unknown removed, Created is default).
ALTER TABLE
  racks
ALTER COLUMN
  controller_state
SET
  DEFAULT '{"state":"created"}' :: jsonb;

-- Migrate any existing rows that still carry the old "unknown" state.
UPDATE
  racks
SET
  controller_state = '{"state":"created"}' :: jsonb
WHERE
  controller_state ->> 'state' = 'unknown';

-- Add rack_id column to switches and power_shelves tables.
-- This creates a direct association between devices and the rack they belong to,
-- mirroring the existing rack_id column on the machines table.
ALTER TABLE
  switches
ADD
  COLUMN IF NOT EXISTS rack_id VARCHAR(64);

ALTER TABLE
  power_shelves
ADD
  COLUMN IF NOT EXISTS rack_id VARCHAR(64);

-- Add firmware_upgrade_job JSONB column to racks table.
ALTER TABLE
  racks
ADD
  COLUMN IF NOT EXISTS firmware_upgrade_job JSONB;

-- Add rack_fw_details JSONB column to machines table.
-- Tracks rack-level firmware upgrade details (task_id, status, start/end time)
-- updated by the rack state machine during firmware upgrades.
ALTER TABLE
  machines
ADD
  COLUMN IF NOT EXISTS rack_fw_details JSONB;