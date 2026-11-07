-- device_info stores the full MlxDeviceInfo JSON blob as reported by
-- the device. Contains part_number, psid, fw_version_current, and
-- other hardware details. Used for firmware config lookup and version
-- matching. Nullable for backwards compatibility with existing rows.
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS device_info JSONB;
-- device_info_ts records when the last device info update was received.
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS device_info_ts TIMESTAMPTZ;
