-- Record rack reprovisioning requests that a power shelf skipped while rack
-- firmware reprovisioning was disabled. A separate column keeps `status` in the
-- shape that earlier binaries decode.
ALTER TABLE power_shelves
    ADD COLUMN rack_firmware_reprovisioning_skip JSONB;
