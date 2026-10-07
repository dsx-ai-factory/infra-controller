-- Add migration script here
ALTER TABLE machines
  ADD COLUMN last_reboot_time TIMESTAMPTZ,
  ADD COLUMN last_cleanup_time TIMESTAMPTZ,
  ADD COLUMN last_discovery_time TIMESTAMPTZ
;
