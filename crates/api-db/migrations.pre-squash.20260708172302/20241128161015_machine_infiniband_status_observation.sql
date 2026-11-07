-- Add infiniband configuration to machines
ALTER TABLE IF EXISTS machines
    -- On startup and machine creation, the infiniband status has not been observed yet.
    -- So it can be NULL by default
    -- It is not required to update existing table records during migration
    ADD COLUMN infiniband_status_observation jsonb NULL
;
