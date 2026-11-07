-- Add ib_partition_state_history table (was missing entirely).
CREATE TABLE IF NOT EXISTS ib_partition_state_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    partition_id uuid NOT NULL,
    state jsonb NOT NULL,
    state_version VARCHAR(64) NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Add keep-limit triggers for all state history tables that were missing them.
-- Caps each entity's history at 250 rows, matching the existing pattern used by
-- machine_state_history, network_segment_state_history, and dpa_interface_state_history.

CREATE OR REPLACE FUNCTION ib_partition_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM ib_partition_state_history WHERE partition_id=NEW.partition_id AND id NOT IN (SELECT id from ib_partition_state_history where partition_id=NEW.partition_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER t_ib_partition_state_history_keep_limit
  AFTER INSERT ON ib_partition_state_history
  FOR EACH ROW EXECUTE PROCEDURE ib_partition_state_history_keep_limit();

CREATE OR REPLACE FUNCTION switch_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM switch_state_history WHERE switch_id=NEW.switch_id AND id NOT IN (SELECT id from switch_state_history where switch_id=NEW.switch_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER t_switch_state_history_keep_limit
  AFTER INSERT ON switch_state_history
  FOR EACH ROW EXECUTE PROCEDURE switch_state_history_keep_limit();

CREATE OR REPLACE FUNCTION rack_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM rack_state_history WHERE rack_id=NEW.rack_id AND id NOT IN (SELECT id from rack_state_history where rack_id=NEW.rack_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER t_rack_state_history_keep_limit
  AFTER INSERT ON rack_state_history
  FOR EACH ROW EXECUTE PROCEDURE rack_state_history_keep_limit();

CREATE OR REPLACE FUNCTION power_shelf_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM power_shelf_state_history WHERE power_shelf_id=NEW.power_shelf_id AND id NOT IN (SELECT id from power_shelf_state_history where power_shelf_id=NEW.power_shelf_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER t_power_shelf_state_history_keep_limit
  AFTER INSERT ON power_shelf_state_history
  FOR EACH ROW EXECUTE PROCEDURE power_shelf_state_history_keep_limit();

CREATE OR REPLACE FUNCTION spdm_machine_attestation_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM spdm_machine_attestation_history WHERE machine_id=NEW.machine_id AND id NOT IN (SELECT id from spdm_machine_attestation_history where machine_id=NEW.machine_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER t_spdm_machine_attestation_history_keep_limit
  AFTER INSERT ON spdm_machine_attestation_history
  FOR EACH ROW EXECUTE PROCEDURE spdm_machine_attestation_history_keep_limit();
