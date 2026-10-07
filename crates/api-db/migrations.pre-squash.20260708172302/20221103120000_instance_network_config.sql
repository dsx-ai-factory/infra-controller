-- Add migration script here

-- Add network configuration to instances
-- This breaks existing instances since we don't set a valid config
ALTER TABLE IF EXISTS instances
    ADD COLUMN network_config_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN network_config jsonb NOT NULL DEFAULT ('{}')
;
