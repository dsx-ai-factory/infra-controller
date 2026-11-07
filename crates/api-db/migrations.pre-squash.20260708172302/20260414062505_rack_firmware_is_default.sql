-- Add is_default flag to rack_firmware table.
-- Defaults to false for existing rows.
ALTER TABLE rack_firmware ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT false;

-- Enforce at most one default per rack_hardware_type.
CREATE UNIQUE INDEX idx_rack_firmware_one_default_per_type
    ON rack_firmware (rack_hardware_type) WHERE is_default = true;
