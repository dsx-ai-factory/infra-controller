-- Add column to store parsed firmware components from the rack firmware JSON
ALTER TABLE rack_firmware ADD COLUMN parsed_components JSONB;

-- Add index for querying parsed components
CREATE INDEX idx_rack_firmware_parsed_components ON rack_firmware USING gin(parsed_components);

