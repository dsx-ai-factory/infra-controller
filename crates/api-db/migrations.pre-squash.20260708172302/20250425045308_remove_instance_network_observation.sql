-- Add migration script here
ALTER TABLE instances
  DROP COLUMN network_status_observation
;
