-- DPF Helm chart type extension services are reconciled asynchronously.
-- Preserve their desired lifecycle separately from API-visible version_ctr so
-- stale controller iterations cannot overwrite a newer request.
ALTER TABLE extension_services
    ADD COLUMN controller_state JSONB,
    ADD COLUMN controller_state_version VARCHAR(64),
    ADD COLUMN controller_state_outcome JSONB DEFAULT NULL;

-- Existing KUBERNETES_POD records have no DPF work to perform. Give them an
-- initialized state so the controller framework and RPC layer can read every
-- row consistently.
UPDATE extension_services
SET controller_state = jsonb_build_object(
        'state',
        CASE WHEN deleted IS NULL THEN 'ready' ELSE 'deleted' END
    ),
    controller_state_version = 'V1-T' || floor(EXTRACT(EPOCH FROM updated) * 1000000)::bigint
WHERE controller_state IS NULL;

-- Require a state for every service after the backfill. The defaults preserve
-- current KUBERNETES_POD behavior for normal inserts; the future DPF Helm
-- create path explicitly writes Creating in the same transaction as the
-- service and its desired data.
ALTER TABLE extension_services
    ALTER COLUMN controller_state SET NOT NULL,
    ALTER COLUMN controller_state_version SET NOT NULL,
    ALTER COLUMN controller_state SET DEFAULT '{"state":"ready"}'::jsonb,
    ALTER COLUMN controller_state_version SET DEFAULT
        ('V1-T' || floor(EXTRACT(EPOCH FROM clock_timestamp()) * 1000000)::bigint);

-- A DPF Helm chart service keeps its deterministic DPUService name while its
-- soft-deleted predecessor is being finalized. Keep that name reserved until
-- the controller records the terminal Deleted state.
CREATE UNIQUE INDEX extension_services_dpf_helm_chart_pending_name_unique
    ON extension_services (tenant_organization_id, lower(name))
    WHERE type = 'dpf_helm_chart'
      AND (deleted IS NULL OR controller_state <> '{"state":"deleted"}'::jsonb);

CREATE TABLE extension_services_controller_iteration_ids (
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE extension_services_controller_queued_objects (
    object_id VARCHAR NOT NULL PRIMARY KEY,
    processed_by TEXT,
    processing_started_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Keep controller state transitions queryable.
CREATE TABLE extension_service_state_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    object_id TEXT NOT NULL,
    state JSONB NOT NULL,
    state_version VARCHAR(64) NOT NULL,
    "timestamp" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX extension_service_state_history_object_id_idx
    ON extension_service_state_history (object_id);

CREATE FUNCTION extension_service_state_history_keep_limit() RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM extension_service_state_history
    WHERE object_id = NEW.object_id
      AND id NOT IN (
          SELECT id
          FROM extension_service_state_history
          WHERE object_id = NEW.object_id
          ORDER BY id DESC
          LIMIT 250
      );
    RETURN NULL;
END;
$$;

CREATE TRIGGER t_extension_service_state_history_keep_limit
AFTER INSERT ON extension_service_state_history
FOR EACH ROW EXECUTE FUNCTION extension_service_state_history_keep_limit();

INSERT INTO extension_service_state_history (object_id, state, state_version, "timestamp")
SELECT id::text, controller_state, controller_state_version, updated
FROM extension_services;
