-- Add migration script here
CREATE TABLE IF NOT EXISTS spx_partitions
(
    id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,

    name VARCHAR(256) NOT NULL DEFAULT (''),
    description VARCHAR(1024) NOT NULL DEFAULT (''),
    tenant_organization_id VARCHAR(64) NOT NULL,
    config_version VARCHAR(64) NOT NULL,
    vni integer NULL,

    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted TIMESTAMPTZ
);

CREATE UNIQUE INDEX vpcs_unique_active_spx_vni ON spx_partitions (vni) WHERE (deleted IS NULL);

ALTER TABLE IF EXISTS instances
    ADD COLUMN IF NOT EXISTS spx_config_version     VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN IF NOT EXISTS spx_config             jsonb       NOT NULL DEFAULT ('{"spx_attachments": []}')
;

ALTER TABLE IF EXISTS dpa_interfaces
   ADD COLUMN IF NOT EXISTS device_description      VARCHAR(256)
;

ALTER TABLE IF EXISTS dpa_interfaces
   DROP COLUMN IF EXISTS network_status_observation;
;

ALTER TABLE IF EXISTS machines
  ADD COLUMN IF NOT EXISTS spx_status_observation jsonb NULL
;
