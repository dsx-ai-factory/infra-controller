ALTER TABLE IF EXISTS machines
    -- Before forge-dpu-agent first runs the network status has not been observed yet,
    -- therefore it is NULL.
    ADD COLUMN network_status_observation jsonb NULL
;
