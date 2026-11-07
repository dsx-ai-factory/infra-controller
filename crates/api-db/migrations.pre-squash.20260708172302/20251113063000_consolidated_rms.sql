-- ============================================================================
-- Consolidated RMS database migration file.
--
-- This merges all RMS-Carbide migrations into a single migration
-- file, taking into consideration migrations that act on the same
-- table.
--
-- Each migration filename, and corresponding changelog, are
-- attached below for reference/history.
--
--------------------------------------------------------------------------------
-- 20250731101010_create_power_shelves.sql
-- 20250808000000_create_switch.sql
--
-- No changelog included for either.
--
--------------------------------------------------------------------------------
-- 20250813120000_add_power_shelf_id_to_machine_interfaces.sql
--
-- This migration is an enhancement on 20250711040508_instance_interface_id.sql
-- and was created in response to https://jirasw.nvidia.com/browse/FORGE-6604.
--
-- The key enhancements here are:
-- 1) It generates a unique UUID for each interface. While technically it only
--    needs to be unique per instance, it's better to have them all globally
--    unique.
-- 2) It dives into each interface element for a given instance (and not just
--    interface[0], supporting VFs and multiple PFs.
--
-- Breakdown is:
--
-- Simple enhancement to use to_jsonb(gen_random_uuid()::text) to generate
-- a unique UUID for the interface, replacing the static UUID being set
-- in the prior migration.
--
-- From there, this does te following:
-- - Uses jsonb_array_elements to get at each element in network_config['interfaces'].
-- - Uses jsonb_set to update the internal_uuid of each array element.
-- - Uses jsonb_agg to smash all of the updated array elements back together.
-- - Feeds that to json_set to update network_config['interfaces'] with the updated
--   list of interfaces.
--
-- For safety, this will also:
-- - Verify that network_config['interfaces'] exists.
-- - Verify that network_config['interfaces'] is an array.
--
-- If the safety checks fail, that row is just ignored (since we use it
-- with a WHERE).
--
--------------------------------------------------------------------------------
-- 20250822230000_expected_machines_add_host_mac.sql
--
-- No changelog included.
--
--------------------------------------------------------------------------------
-- 20250907120000_add_rack_id_expected.sql
--
-- There is no expected_racks table, since there is no real rack entity that
-- will show up on the network and be identifiable. The current chassis SN on
-- the trays may or may not be reliably programmed. The expected_machines,
-- expected_nvlink_switches, expected_power_shelves tables will have an optional
-- arbitrary rack id for each row that groups those items into a "rack". Until
-- kyber, where there is a midplane and we can query the actual "rack". We create
-- a racks table and create the derived rack objects based on the expected items
-- above. Expected topology of the trays in the rack is not specified or handled yet.
--
--------------------------------------------------------------------------------
-- 20250909004142_add_ip_address_to_expected_power_shelves.sql
--
-- No changelog included.
--
--------------------------------------------------------------------------------
-- 20250910160300_add_switch_id_to_machine_interfaces.sql
--
-- Add switch_id column to machine_interfaces table.
-- This column references the machines table to establish a relationship with switches
-- Note: machine IDs are now VARCHAR(64) as per the stable_machine_id_only migration
-- TODO Should probably be a list of ids
--
--------------------------------------------------------------------------------
-- 20251010061200_add_rack_id_to_expected_switch.sql
--
-- Add rack_id column to expected_switches table
-- This column references the racks table to establish a relationship with switches
--
--------------------------------------------------------------------------------
-- 20251106000000_create_rack_controller_lock.sql
--
-- No changelog included.

-- =============================================================================
-- Power Shelves Management Tables
-- From: 20250731101010_create_power_shelves.sql
-- =============================================================================

-- Create power_shelves table.
CREATE TABLE power_shelves (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR NOT NULL UNIQUE,
    config JSONB NOT NULL,
    status JSONB,
    deleted TIMESTAMP WITH TIME ZONE,
    controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    controller_state jsonb NOT NULL DEFAULT ('{"state":"initializing"}'),
    controller_state_outcome JSONB,
    created TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Create expected_power_shelves table.
-- Consolidation notes: includes rack_id and ip_address from later migrations
-- (
--   20250907120000_add_rack_id_expected.sql,
--   20250909004142_add_ip_address_to_expected_power_shelves.sql
-- )
CREATE TABLE expected_power_shelves (
    serial_number VARCHAR(32) NOT NULL,
    bmc_mac_address macaddr NOT NULL UNIQUE,
    bmc_username VARCHAR(16) NOT NULL,
    bmc_password VARCHAR(16) NOT NULL,
    metadata_name VARCHAR(256) NOT NULL DEFAULT (''),
    metadata_description VARCHAR(1024) NOT NULL DEFAULT (''),
    metadata_labels JSONB NOT NULL DEFAULT ('{}'),
    host_name VARCHAR(256),
    rack_id VARCHAR(64),
    ip_address inet
);

-- Create power_shelf_controller_lock table.
CREATE TABLE power_shelf_controller_lock(id uuid DEFAULT gen_random_uuid() NOT NULL);

-- Create power_shelf_state_history table.
CREATE TABLE power_shelf_state_history (
    id BIGSERIAL PRIMARY KEY,
    power_shelf_id VARCHAR(64) NOT NULL,
    state JSONB NOT NULL,
    state_version VARCHAR(64) NOT NULL,
    timestamp TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Create indexes for power shelf tables.
CREATE INDEX idx_power_shelf_state_history_power_shelf_id ON power_shelf_state_history(power_shelf_id);
CREATE INDEX idx_power_shelf_state_history_timestamp ON power_shelf_state_history(timestamp);
CREATE INDEX idx_power_shelves_deleted ON power_shelves(deleted);
CREATE INDEX idx_power_shelves_name ON power_shelves ((config ->> 'name'));
CREATE INDEX idx_expected_power_shelves_ip_address ON expected_power_shelves(ip_address);

-- Add foreign key constraint for power_shelf_state_history.
ALTER TABLE power_shelf_state_history
ADD CONSTRAINT fk_power_shelf_state_history_power_shelf_id
    FOREIGN KEY (power_shelf_id) REFERENCES power_shelves(id) ON DELETE CASCADE;

-- Add comment to document the ip_address field.
COMMENT ON COLUMN expected_power_shelves.ip_address IS 'IP address of the power shelf BMC interface';

-- Create trigger to update power shelf updated timestamp.
CREATE OR REPLACE FUNCTION update_power_shelves_updated()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_power_shelves_updated
BEFORE UPDATE ON power_shelves
FOR EACH ROW EXECUTE FUNCTION update_power_shelves_updated();

-- =============================================================================
-- Switch Management Tables
-- From: 20250808000000_create_switch.sql
-- Note: rack_id removed in 20251010061200_add_rack_id_to_expected_switch.sql
-- =============================================================================

-- Create switches table (rack_id column excluded - it was added then removed).
CREATE TABLE switches (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR NOT NULL UNIQUE,
    config JSONB NOT NULL,
    status JSONB,
    deleted TIMESTAMP WITH TIME ZONE,
    controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    controller_state jsonb NOT NULL DEFAULT ('{"state":"initializing"}'),
    controller_state_outcome JSONB,
    created TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Create expected_switches table.
-- Consolidation notes: includes rack_id, excludes ip_address (was added then removed)
-- (20251010061200_add_rack_id_to_expected_switch.sql)
CREATE TABLE expected_switches (
    serial_number VARCHAR(32) NOT NULL,
    bmc_mac_address macaddr NOT NULL UNIQUE,
    bmc_username VARCHAR(16) NOT NULL,
    bmc_password VARCHAR(16) NOT NULL,
    metadata_name VARCHAR(256) NOT NULL DEFAULT (''),
    metadata_description VARCHAR(1024) NOT NULL DEFAULT (''),
    metadata_labels JSONB NOT NULL DEFAULT ('{}'),
    rack_id VARCHAR(64)
);

-- Create switch_controller_lock table.
CREATE TABLE switch_controller_lock(id uuid DEFAULT gen_random_uuid() NOT NULL);

-- Create switch_state_history table.
CREATE TABLE switch_state_history (
    id BIGSERIAL PRIMARY KEY,
    switch_id VARCHAR(64) NOT NULL,
    state JSONB NOT NULL,
    state_version VARCHAR(64) NOT NULL,
    timestamp TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Create indexes for switch tables.
CREATE INDEX idx_switch_state_history_switch_id ON switch_state_history(switch_id);
CREATE INDEX idx_switch_state_history_timestamp ON switch_state_history(timestamp);
CREATE INDEX idx_switches_deleted ON switches(deleted);
CREATE INDEX idx_switches_name ON switches ((config ->> 'name'));
CREATE INDEX idx_expected_switches_rack_id ON expected_switches(rack_id);

-- Add foreign key constraint for switch_state_history.
ALTER TABLE switch_state_history
ADD CONSTRAINT fk_switch_state_history_switch_id
    FOREIGN KEY (switch_id) REFERENCES switches(id) ON DELETE CASCADE;

-- Create trigger to update switch updated timestamp.
CREATE OR REPLACE FUNCTION update_switches_updated()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_switches_updated
BEFORE UPDATE ON switches
FOR EACH ROW EXECUTE FUNCTION update_switches_updated();

-- =============================================================================
-- Changes to Existing Carbide Machine Interfaces Table
-- From: 20250813120000_add_power_shelf_id_to_machine_interfaces.sql
--       20250910160300_add_switch_id_to_machine_interfaces.sql
-- =============================================================================

-- Add power_shelf_id column to machine_interfaces table.
ALTER TABLE machine_interfaces
ADD COLUMN IF NOT EXISTS power_shelf_id VARCHAR(64) REFERENCES machines(id);

-- Add switch_id column to machine_interfaces table.
ALTER TABLE machine_interfaces
ADD COLUMN IF NOT EXISTS switch_id VARCHAR(64) REFERENCES machines(id);

-- Create indexes against power_shelf_id and switch_id.
CREATE INDEX idx_machine_interfaces_power_shelf_id ON machine_interfaces(power_shelf_id);
CREATE INDEX idx_machine_interfaces_switch_id ON machine_interfaces(switch_id);

-- =============================================================================
-- Changes to Existing Carbide Expected Machines Table
-- From: 20250822230000_expected_machines_add_host_mac.sql
--       20250907120000_add_rack_id_expected.sql
-- =============================================================================

-- Add host_nics column.
ALTER TABLE expected_machines
ADD COLUMN IF NOT EXISTS host_nics jsonb NOT NULL DEFAULT '[]'::jsonb;

-- Add rack_id column.
ALTER TABLE expected_machines
ADD COLUMN IF NOT EXISTS rack_id VARCHAR(64);

-- =============================================================================
-- Rack Management Tables
-- From: 20250907120000_add_rack_id_expected.sql
--       20251106000000_create_rack_controller_lock.sql
-- =============================================================================

CREATE TABLE racks (
    id VARCHAR(64) PRIMARY KEY,
    config JSONB NOT NULL DEFAULT ('{}'),
    controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    controller_state JSONB NOT NULL DEFAULT ('{"state":"unknown"}'),
    controller_state_outcome JSONB,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted TIMESTAMPTZ
);

CREATE TABLE rack_state_history (
    id BIGSERIAL PRIMARY KEY,
    rack_id VARCHAR(64) NOT NULL,
    state JSONB NOT NULL,
    state_version VARCHAR(64) NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create indexes.
CREATE INDEX idx_rack_state_history_rack_id ON rack_state_history(rack_id);
CREATE INDEX idx_rack_state_history_timestamp ON rack_state_history(timestamp);
CREATE INDEX idx_racks_deleted ON racks(deleted);

-- Add foreign key constraint for rack_state_history.
ALTER TABLE rack_state_history
ADD CONSTRAINT fk_rack_state_history_rack_id
    FOREIGN KEY (rack_id) REFERENCES racks(id) ON DELETE CASCADE;

-- Create trigger to update rack updated timestamp.
CREATE OR REPLACE FUNCTION update_racks_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_racks_timestamp
BEFORE UPDATE ON racks
FOR EACH ROW EXECUTE FUNCTION update_racks_timestamp();

CREATE TABLE IF NOT EXISTS rack_controller_lock(id uuid DEFAULT gen_random_uuid() NOT NULL);
