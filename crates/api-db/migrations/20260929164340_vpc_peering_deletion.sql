-- Keep peering ownership until DPUs acknowledge permission removal.
ALTER TABLE vpc_peerings
    ADD COLUMN deletion_version TEXT,
    ADD COLUMN controller_state_outcome JSONB;

CREATE TABLE vpc_peerings_controller_iteration_ids (
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE vpc_peerings_controller_queued_objects (
    object_id TEXT PRIMARY KEY,
    processed_by TEXT,
    processing_started_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
