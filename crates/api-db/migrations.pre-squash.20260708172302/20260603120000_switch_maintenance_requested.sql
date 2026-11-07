-- Add switch_maintenance_requested column to switches table.
-- switch_maintenance_requested: when set by an external entity, the state controller
-- (when switch is Ready or Error) transitions to Maintenance with the requested
-- operation (PowerOn / PowerOff / Reset). Mirrors power_shelves.power_shelf_maintenance_requested.
ALTER TABLE
    switches
ADD
    COLUMN switch_maintenance_requested JSONB;
