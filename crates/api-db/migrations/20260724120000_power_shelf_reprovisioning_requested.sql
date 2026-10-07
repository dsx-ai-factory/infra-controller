-- Add power_shelf_reprovisioning_requested and firmware_upgrade_status columns
-- to power_shelves. Mirrors switches.switch_reprovisioning_requested /
-- firmware_upgrade_status for rack-level firmware upgrade wait phases.

ALTER TABLE power_shelves
    ADD COLUMN power_shelf_reprovisioning_requested JSONB,
    ADD COLUMN firmware_upgrade_status JSONB;
