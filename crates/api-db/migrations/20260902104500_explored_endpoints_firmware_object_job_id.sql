-- Track the backend firmware-object job ID for a pre-ingestion compute tray
-- flashed via --bypass-state-controller. The tray has no machines row, so its
-- job ID is persisted here (keyed by BMC IP) instead, letting get_firmware_status
-- fall back to the DB and keep querying the backend after a nico-api restart.
ALTER TABLE explored_endpoints
    ADD COLUMN backend_firmware_object_job_id TEXT;
