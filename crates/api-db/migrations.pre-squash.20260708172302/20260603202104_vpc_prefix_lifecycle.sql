-- Add durable lifecycle state and a soft-delete marker for asynchronous cleanup.
ALTER TABLE network_vpc_prefixes
    ADD COLUMN deleted TIMESTAMPTZ DEFAULT NULL,
    ADD COLUMN controller_state jsonb,
    ADD COLUMN controller_state_version VARCHAR(64),
    ADD COLUMN controller_state_outcome jsonb DEFAULT NULL;

-- Keep controller state transitions queryable without overloading the prefix row.
CREATE TABLE vpc_prefix_state_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    vpc_prefix_id uuid NOT NULL,
    state jsonb NOT NULL,
    state_version VARCHAR(64) NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- History intentionally omits a foreign key so records can survive VPC-prefix hard-delete.
CREATE INDEX vpc_prefix_state_history_vpc_prefix_id_idx
    ON vpc_prefix_state_history(vpc_prefix_id);

CREATE OR REPLACE FUNCTION vpc_prefix_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM vpc_prefix_state_history WHERE vpc_prefix_id=NEW.vpc_prefix_id AND id NOT IN (SELECT id from vpc_prefix_state_history where vpc_prefix_id=NEW.vpc_prefix_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

-- Match the state-controller history retention limit used by other lifecycle tables.
CREATE TRIGGER t_vpc_prefix_state_history_keep_limit
  AFTER INSERT ON vpc_prefix_state_history
  FOR EACH ROW EXECUTE PROCEDURE vpc_prefix_state_history_keep_limit();

-- Existing prefixes were already usable before this controller existed, so backfill them as ready.
UPDATE network_vpc_prefixes
SET controller_state = '{"state":"ready"}'::jsonb,
    controller_state_version = 'V1-T1666644937952268';

-- Seed one history row per upgraded prefix so lifecycle views have an initial state.
INSERT INTO vpc_prefix_state_history (vpc_prefix_id, state, state_version)
SELECT id, controller_state, controller_state_version
FROM network_vpc_prefixes;

-- New prefixes created after the migration start in provisioning until the controller observes them.
ALTER TABLE network_vpc_prefixes
    ALTER COLUMN controller_state SET NOT NULL,
    ALTER COLUMN controller_state SET DEFAULT ('{"state":"provisioning"}'::jsonb),
    ALTER COLUMN controller_state_version SET NOT NULL,
    ALTER COLUMN controller_state_version SET DEFAULT ('V1-T1666644937952268');

-- State-controller bookkeeping tables. Names must match VpcPrefixStateControllerIO constants.
CREATE TABLE network_vpc_prefixes_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE network_vpc_prefixes_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    processed_by TEXT NULL DEFAULT NULL,
    processing_started_at timestamptz NOT NULL DEFAULT NOW()
);
