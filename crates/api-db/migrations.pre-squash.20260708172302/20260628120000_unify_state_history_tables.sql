-- State history must outlive the object it describes. Remove the remaining
-- cascading foreign keys before normalizing the object ID columns.
ALTER TABLE power_shelf_state_history
    DROP CONSTRAINT fk_power_shelf_state_history_power_shelf_id;
ALTER TABLE rack_state_history
    DROP CONSTRAINT fk_rack_state_history_rack_id;
ALTER TABLE switch_state_history
    DROP CONSTRAINT fk_switch_state_history_switch_id;

-- Older power-shelf and switch history rows allowed explicit NULL timestamps.
-- Preserve those rows while making the timestamp contract consistent.
UPDATE power_shelf_state_history SET timestamp = NOW() WHERE timestamp IS NULL;
UPDATE switch_state_history SET timestamp = NOW() WHERE timestamp IS NULL;

ALTER TABLE power_shelf_state_history
    ALTER COLUMN timestamp SET NOT NULL;
ALTER TABLE switch_state_history
    ALTER COLUMN timestamp SET NOT NULL;

-- Give every state-history table the same TEXT object key.
ALTER TABLE machine_state_history
    RENAME COLUMN machine_id TO object_id;
ALTER TABLE machine_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE network_segment_state_history
    RENAME COLUMN segment_id TO object_id;
ALTER TABLE network_segment_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE vpc_prefix_state_history
    RENAME COLUMN vpc_prefix_id TO object_id;
ALTER TABLE vpc_prefix_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE dpa_interface_state_history
    RENAME COLUMN interface_id TO object_id;
ALTER TABLE dpa_interface_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE ib_partition_state_history
    RENAME COLUMN partition_id TO object_id;
ALTER TABLE ib_partition_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE power_shelf_state_history
    RENAME COLUMN power_shelf_id TO object_id;
ALTER TABLE power_shelf_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE rack_state_history
    RENAME COLUMN rack_id TO object_id;
ALTER TABLE rack_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

ALTER TABLE switch_state_history
    RENAME COLUMN switch_id TO object_id;
ALTER TABLE switch_state_history
    ALTER COLUMN object_id TYPE TEXT USING object_id::TEXT;

-- Keep object-ID lookups indexed consistently across every history table.
ALTER INDEX machine_state_history_machine_id_idx
    RENAME TO machine_state_history_object_id_idx;
CREATE INDEX network_segment_state_history_object_id_idx
    ON network_segment_state_history(object_id);
ALTER INDEX vpc_prefix_state_history_vpc_prefix_id_idx
    RENAME TO vpc_prefix_state_history_object_id_idx;
CREATE INDEX dpa_interface_state_history_object_id_idx
    ON dpa_interface_state_history(object_id);
CREATE INDEX ib_partition_state_history_object_id_idx
    ON ib_partition_state_history(object_id);
ALTER INDEX idx_power_shelf_state_history_power_shelf_id
    RENAME TO power_shelf_state_history_object_id_idx;
ALTER INDEX idx_rack_state_history_rack_id
    RENAME TO rack_state_history_object_id_idx;
ALTER INDEX idx_switch_state_history_switch_id
    RENAME TO switch_state_history_object_id_idx;

-- Recreate the retention functions against the common object_id column.
-- Serialize cleanup per table/object so concurrent inserts cannot exceed the limit.
CREATE OR REPLACE FUNCTION machine_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM machine_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM machine_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION network_segment_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM network_segment_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM network_segment_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION vpc_prefix_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM vpc_prefix_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM vpc_prefix_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION dpa_interface_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM dpa_interface_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM dpa_interface_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION ib_partition_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM ib_partition_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM ib_partition_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION power_shelf_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM power_shelf_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM power_shelf_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION rack_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM rack_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM rack_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION switch_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM switch_state_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id FROM switch_state_history WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

-- Machine cleanup deletes DPA-interface rows, but their history is durable.
CREATE OR REPLACE PROCEDURE cleanup_machine_by_id(deletion_machine_id VARCHAR(64))
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE machine_interfaces SET machine_id = NULL WHERE machine_id = deletion_machine_id;
    UPDATE machine_interfaces SET attached_dpu_machine_id = NULL WHERE attached_dpu_machine_id = deletion_machine_id;
    DELETE FROM measurement_journal WHERE report_id IN (SELECT report_id FROM measurement_reports WHERE machine_id = deletion_machine_id);
    DELETE FROM measurement_reports_values WHERE report_id IN (SELECT report_id FROM measurement_reports WHERE machine_id = deletion_machine_id);
    DELETE FROM measurement_reports WHERE machine_id = deletion_machine_id;
    DELETE FROM measurement_approved_machines WHERE machine_id = deletion_machine_id;
    DELETE FROM machine_topologies WHERE machine_id = deletion_machine_id;
    DELETE FROM machine_validation WHERE machine_id = deletion_machine_id;
    DELETE FROM dpa_interfaces WHERE machine_id = deletion_machine_id;
    DELETE FROM applied_dpu_remediations WHERE dpu_machine_id = deletion_machine_id;
    DELETE FROM machines WHERE id = deletion_machine_id;
END
$$;
