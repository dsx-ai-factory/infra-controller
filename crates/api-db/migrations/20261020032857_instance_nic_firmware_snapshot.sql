-- Allocation firmware intent must survive replacement of instance configuration.
ALTER TABLE instances ADD COLUMN nic_firmware_snapshot JSONB;
