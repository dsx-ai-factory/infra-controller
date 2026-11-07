-- Add migration script here
ALTER TABLE expected_machines ADD COLUMN dpf_enabled BOOLEAN NOT NULL DEFAULT TRUE;