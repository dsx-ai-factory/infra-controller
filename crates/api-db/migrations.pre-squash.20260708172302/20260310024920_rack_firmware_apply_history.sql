CREATE TABLE rack_firmware_apply_history(
    id BIGSERIAL PRIMARY KEY,
    firmware_id VARCHAR(256) NOT NULL,
    rack_id VARCHAR(256) NOT NULL,
    firmware_type VARCHAR(64) NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_rack_firmware_apply_history_firmware_id ON rack_firmware_apply_history(firmware_id);
CREATE INDEX idx_rack_firmware_apply_history_rack_id ON rack_firmware_apply_history(rack_id);
CREATE INDEX idx_rack_firmware_apply_history_applied_at ON rack_firmware_apply_history(applied_at);
