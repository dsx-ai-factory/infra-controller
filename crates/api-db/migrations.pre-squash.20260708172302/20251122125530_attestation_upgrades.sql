-- Add migration script here
ALTER TABLE spdm_machine_devices_attestation
    ADD COLUMN state_version VARCHAR DEFAULT 'V1-T175698206507492',
    ADD COLUMN state_outcome JSONB
;

ALTER TABLE spdm_machine_devices_attestation
    DROP COLUMN last_known_metadata;

ALTER TABLE spdm_machine_devices_attestation
    RENAME COLUMN current_metadata TO metadata;

CREATE TABLE attestation_controller_iteration_ids(
    id BIGSERIAL PRIMARY KEY,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

CREATE TABLE attestation_controller_queued_objects(
    object_id VARCHAR PRIMARY KEY,
    iteration_id BIGINT
);

ALTER TABLE spdm_machine_attestation_history
    DROP COLUMN state_version,
    DROP COLUMN attestation_status;
