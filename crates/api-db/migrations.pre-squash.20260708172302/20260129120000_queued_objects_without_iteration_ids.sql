-- Removes iteration_id column from all queued_objects tables

ALTER TABLE machine_state_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE network_segments_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE ib_partition_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE dpa_interfaces_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE power_shelf_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE switch_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE rack_controller_queued_objects
    DROP COLUMN iteration_id;

ALTER TABLE attestation_controller_queued_objects
    DROP COLUMN iteration_id;
