-- Add rack_hardware_type to rack_firmware table.
-- Defaults existing rows to 'any' (matches any rack hardware type).
ALTER TABLE rack_firmware ADD COLUMN rack_hardware_type TEXT NOT NULL DEFAULT 'any';
CREATE INDEX idx_rack_firmware_rack_hardware_type ON rack_firmware(rack_hardware_type);

-- Add rack_hardware_type to rack_firmware_apply_history table.
ALTER TABLE rack_firmware_apply_history ADD COLUMN rack_hardware_type TEXT NOT NULL DEFAULT 'any';
