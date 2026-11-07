-- Add migration script here
ALTER TABLE instances
  DROP COLUMN managed_resource_id
;
