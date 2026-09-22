-- Remember the first protection request and the controller's last result.
ALTER TABLE site_prefixes
    ADD COLUMN isolation_requested_at TIMESTAMPTZ,
    ADD COLUMN controller_state_outcome JSONB;

CREATE TABLE site_prefixes_controller_iteration_ids (
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE site_prefixes_controller_queued_objects (
    object_id TEXT PRIMARY KEY,
    processed_by TEXT,
    processing_started_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
