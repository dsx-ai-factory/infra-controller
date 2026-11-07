-- delete existing "legacy" rows with nmx_m_id and rename the field to nmx_c_partition_id
DELETE FROM nvlink_partitions;

ALTER TABLE nvlink_partitions
    DROP CONSTRAINT IF EXISTS nvlink_partitions_nmx_m_id_key;

ALTER TABLE nvlink_partitions
    RENAME COLUMN nmx_m_id TO nmx_c_partition_id;

ALTER TABLE nvlink_partitions
    ALTER COLUMN nmx_c_partition_id TYPE INTEGER
    USING nmx_c_partition_id::INTEGER;
