-- Add last_hb_time column, which is a timestamp of when we last sent a heartbeat command to the DPA
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS last_hb_time TIMESTAMPTZ NOT NULL DEFAULT NOW();
