-- RMS owns firmware object storage and apply history. Drop the legacy BMM-local
-- tables after the migration to RMS-backed firmware objects.
DROP TABLE IF EXISTS rack_firmware_apply_history;
DROP TABLE IF EXISTS rack_firmware;
