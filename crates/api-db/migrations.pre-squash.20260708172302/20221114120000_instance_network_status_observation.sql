-- Add migration script here

-- Add network configuration to instances
-- This breaks existing instances since we don't set a valid config
ALTER TABLE IF EXISTS instances
    -- On startup and instance creation, the network status has not been observed yet.
    -- Therefore it is `null`.
    ADD COLUMN network_status_observation jsonb NOT NULL DEFAULT ('null')
;
