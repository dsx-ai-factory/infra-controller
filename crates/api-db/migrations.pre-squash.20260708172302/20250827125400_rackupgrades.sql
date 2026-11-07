ALTER TABLE machines
    ADD COLUMN firmware_update_time_window_start TIMESTAMPTZ,
    ADD COLUMN firmware_update_time_window_end TIMESTAMPTZ,
    ADD COLUMN update_complete BOOLEAN DEFAULT false;

ALTER TABLE desired_firmware
    ADD COLUMN explicit_update_start_needed BOOLEAN NOT NULL DEFAULT false;
