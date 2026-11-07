-- Add migration script here

-- Add infiniband configuration to instances
-- This breaks existing instances since we don't set a valid config
ALTER TABLE IF EXISTS instances
    ADD COLUMN ib_config_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN ib_config jsonb NOT NULL DEFAULT ('{"ib_interfaces": []}'),
    ADD COLUMN ib_status_observation jsonb NOT NULL DEFAULT ('{"ib_config_version": "V1-T1666644937952267", "observed_at": "2023-01-01 00:00:00.000000+00"}')
;

