-- Add migration script here
-- add a new column with default value
ALTER TABLE spdm_machine_devices_attestation ADD COLUMN job_id uuid NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';

ALTER TABLE spdm_machine_devices_attestation DROP CONSTRAINT spdm_machine_devices_attestation_pkey;
ALTER TABLE spdm_machine_devices_attestation ADD PRIMARY KEY (machine_id, device_id, job_id);

-- TODO: add trigger to clean up all old attestations