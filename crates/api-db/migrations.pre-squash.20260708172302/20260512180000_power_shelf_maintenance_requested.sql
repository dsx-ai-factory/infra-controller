-- Add power_shelf_maintenance_requested column to power_shelves table.
-- power_shelf_maintenance_requested: when set by an external entity, the state controller
-- (when power shelf is Ready) transitions to Maintenance with the requested operation
-- (PowerOn / PowerOff). Mirrors the switches.switch_reprovisioning_requested pattern.
ALTER TABLE
    power_shelves
ADD
    COLUMN power_shelf_maintenance_requested JSONB;
