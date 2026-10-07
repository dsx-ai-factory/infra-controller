-- Add migration script here

-- Add state controller owned fields to network segments
ALTER TABLE IF EXISTS network_segments
    ADD COLUMN controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    ADD COLUMN controller_state jsonb NOT NULL DEFAULT ('{"state":"provisioning"}')
;

-- Lock for synchronizing access to network segments
CREATE TABLE network_segments_controller_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);
