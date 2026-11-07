-- Add two columns to hold the underlay and overlay ip addresses of a DPA object
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS underlay_ip inet;
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS overlay_ip inet;
