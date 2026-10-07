-- Add migration script here
ALTER TABLE machines
    ADD COLUMN last_reboot_requested JSONB NULL
;

