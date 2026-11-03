-- Keep operator failure requests durable while the BMC task is being cleaned up.
ALTER TABLE spdm_machine_devices_attestation ADD COLUMN fail_requested_at TIMESTAMPTZ;
