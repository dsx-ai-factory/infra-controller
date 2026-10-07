ALTER TABLE IF EXISTS instances
    ADD COLUMN IF NOT EXISTS nvlink_config_version     VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN IF NOT EXISTS nvlink_config             jsonb       NOT NULL DEFAULT ('{"nvlink_gpus": []}')
;

ALTER TABLE IF EXISTS machines
    ADD COLUMN IF NOT EXISTS nvlink_info jsonb NULL,
    ADD COLUMN IF NOT EXISTS nvlink_status_observation jsonb NULL
;

CREATE TABLE IF NOT EXISTS nvlink_logical_partitions
(
    id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,

    name VARCHAR(256) NOT NULL DEFAULT (''),
    description VARCHAR(1024) NOT NULL DEFAULT (''),
    tenant_organization_id VARCHAR(64) NOT NULL,
    config_version VARCHAR(64) NOT NULL,
    partition_state jsonb NOT NULL DEFAULT ('{"state":"provisioning"}'),

    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS nvlink_partitions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    -- system unique identifier for partition returned by NMX-M
    nmx_m_id VARCHAR NOT NULL UNIQUE,
    name VARCHAR(64) NOT NULL DEFAULT (''),
    domain_uuid uuid NOT NULL,


    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted TIMESTAMPTZ,

    -- logical partition this nvl_partition is part of
    logical_partition_id uuid  NULL,

    PRIMARY KEY(id),
    FOREIGN KEY(logical_partition_id) REFERENCES nvlink_logical_partitions(id)
);

CREATE TABLE nvlink_partition_monitor_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);
