-- Renames ib_subnets to ib_partitions

ALTER TABLE ib_subnets RENAME TO ib_partitions;
ALTER TABLE ibsubnet_controller_lock RENAME TO ib_partition_controller_lock;
