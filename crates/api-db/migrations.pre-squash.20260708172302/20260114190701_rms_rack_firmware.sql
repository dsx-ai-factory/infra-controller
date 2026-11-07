-- Create table to store Rack firmware configurations
CREATE TABLE rack_firmware(
    id VARCHAR(256) PRIMARY KEY,
    config JSONB NOT NULL,
    available BOOLEAN NOT NULL DEFAULT false,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for faster lookups on available configs
CREATE INDEX idx_rack_firmware_available ON rack_firmware(available);
CREATE INDEX idx_rack_firmware_created ON rack_firmware(created);

