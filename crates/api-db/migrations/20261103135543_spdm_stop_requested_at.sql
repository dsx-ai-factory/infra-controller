-- Keep operator stop requests durable while the BMC task is being cleaned up.
ALTER TABLE spdm_machine_devices_attestation ADD COLUMN stop_requested_at TIMESTAMPTZ;
