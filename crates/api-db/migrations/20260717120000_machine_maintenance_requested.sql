-- Add machine_maintenance_requested column to machines table.
-- machine_maintenance_requested: when set by an external entity, the state controller
-- transitions the host from Ready (or Failed) into Maintenance to execute the requested
-- operation (PowerOn / PowerOff / Reset). Mirrors switches.switch_maintenance_requested.

ALTER TABLE machines
    ADD COLUMN machine_maintenance_requested JSONB;
