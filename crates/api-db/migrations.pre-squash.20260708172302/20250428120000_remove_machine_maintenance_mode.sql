-- Remove maintenance mode related fields
-- Maintenance is now set as a health alert

ALTER TABLE machines
  DROP COLUMN maintenance_reference,
  DROP COLUMN maintenance_start_time;
