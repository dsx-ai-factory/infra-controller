-- Introduces processed_by columns for all queued object tables

ALTER TABLE machine_state_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE network_segments_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE ib_partition_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE dpa_interfaces_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE power_shelf_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE switch_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE rack_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();

ALTER TABLE attestation_controller_queued_objects
    ADD COLUMN processed_by TEXT NULL DEFAULT NULL,
    ADD COLUMN processing_started_at timestamptz NOT NULL DEFAULT NOW();