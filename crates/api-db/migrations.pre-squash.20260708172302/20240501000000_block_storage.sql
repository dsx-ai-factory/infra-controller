-- Add migration script here
-- Add storage configuration to instances (similar to 20230630000000_instance_ib_config.sql)
ALTER TABLE IF EXISTS instances
    ADD COLUMN IF NOT EXISTS storage_config_version     VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN IF NOT EXISTS storage_config             jsonb       NOT NULL DEFAULT ('{"volumes": []}'),
    ADD COLUMN IF NOT EXISTS storage_status_observation jsonb       NOT NULL DEFAULT ('{"storage_config_version": "V1-T1666644937952267", "observed_at": "2023-01-01 00:00:00.000000+00"}')
;

-- Add new tables for storage objects
CREATE TABLE IF NOT EXISTS storage_clusters
(
    id          uuid PRIMARY KEY,
    name        VARCHAR(64) NOT NULL,
    description TEXT,
    host        jsonb,
    port        SMALLINT,
    capacity    BIGINT,
    allocated   BIGINT,
    available   BIGINT,
    healthy     BOOLEAN,
    created_at  VARCHAR(64),
    modified_at VARCHAR(64)
);

CREATE TYPE nvmesh_raid_levels AS ENUM ('Concatenated', 'Raid0', 'Raid1', 'Raid10', 'ErasureCoding');

CREATE TABLE IF NOT EXISTS storage_pools
(
    id              uuid PRIMARY KEY,
    name            VARCHAR(64),
    description     TEXT,
    raid_level      nvmesh_raid_levels,
    capacity        BIGINT,
    allocated       BIGINT,
    available       BIGINT,
    organization_id TEXT        NOT NULL,
    use_for_boot_volumes BOOLEAN,
    nvmesh_uuid     uuid,
    cluster_id      uuid        NOT NULL,
    created_at      VARCHAR(64),
    modified_at     VARCHAR(64),
    FOREIGN KEY (cluster_id) REFERENCES storage_clusters (id)
);

CREATE TABLE IF NOT EXISTS storage_volumes
(
    id                   uuid PRIMARY KEY,
    name                 VARCHAR(64),
    description          TEXT,
    capacity             BIGINT,
    delete_with_instance BOOLEAN NOT NULL DEFAULT false,
    boot_volume          BOOLEAN NOT NULL DEFAULT false,
    pool_id              uuid    NOT NULL,
    cluster_id           uuid    NOT NULL,
    nvmesh_uuid          uuid,
    os_image_id          uuid,
    source_id            uuid,
    instance_id          jsonb,
    dpu_machine_id       jsonb,
    health               VARCHAR(64),
    attached             BOOLEAN,
    status_message       VARCHAR(64),
    created_at           VARCHAR(64),
    modified_at          VARCHAR(64),
    FOREIGN KEY (pool_id) REFERENCES storage_pools (id),
    FOREIGN KEY (cluster_id) REFERENCES storage_clusters (id)
);

CREATE TABLE IF NOT EXISTS os_images
(
    id              uuid PRIMARY KEY,
    name            VARCHAR(64),
    description     TEXT,
    source_url      TEXT NOT NULL,
    digest          TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    auth_type       VARCHAR(64),
    auth_token      TEXT,
    rootfs_id       VARCHAR(64),
    rootfs_label    VARCHAR(64),
    boot_disk       VARCHAR(64),
    capacity        BIGINT,
    volume_id       uuid,
    status          VARCHAR(64),
    status_message  VARCHAR(64),
    created_at      VARCHAR(64),
    modified_at     VARCHAR(64),
    FOREIGN KEY (volume_id) REFERENCES storage_volumes (id)
);
