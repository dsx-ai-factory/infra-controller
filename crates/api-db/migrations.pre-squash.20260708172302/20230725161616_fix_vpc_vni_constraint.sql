--- VPCs are deleted by filling in their 'deleted' column with a date/time,
--- and releasing this VNI back to the pool. The VNI will be re-used for
--- later VPCs. That means we can't have a simple unique constraint on the column.

ALTER TABLE vpcs DROP CONSTRAINT vpcs_vni_key;

CREATE UNIQUE INDEX vpcs_unique_active_vni ON vpcs (vni) WHERE (deleted IS NULL);
