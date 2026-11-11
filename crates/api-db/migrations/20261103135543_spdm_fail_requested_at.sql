-- Keep the latest requested outcome separate from the cleanup deadline.
ALTER TABLE spdm_machine_devices_attestation
    ADD COLUMN cancellation_target TEXT CHECK (cancellation_target IN ('waive', 'fail')),
    ADD COLUMN cancellation_cleanup_started_at TIMESTAMPTZ;

-- The existing cancelled_at column records waiver requests from older writers.
UPDATE spdm_machine_devices_attestation
SET cancellation_target = 'waive', cancellation_cleanup_started_at = cancelled_at
WHERE cancelled_at IS NOT NULL;
