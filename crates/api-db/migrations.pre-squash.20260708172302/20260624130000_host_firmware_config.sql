CREATE TABLE host_firmware_config (
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    config JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (vendor, model),
    CONSTRAINT host_firmware_config_config_is_object
        CHECK (jsonb_typeof(config) = 'object')
);

CREATE UNIQUE INDEX host_firmware_config_vendor_model_lower_uidx
    ON host_firmware_config (vendor, lower(model));

CREATE INDEX host_firmware_config_updated_at_idx
    ON host_firmware_config (updated_at);

CREATE OR REPLACE FUNCTION update_host_firmware_config_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at := NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_host_firmware_config_updated_at
BEFORE UPDATE ON host_firmware_config
FOR EACH ROW
EXECUTE FUNCTION update_host_firmware_config_updated_at();
