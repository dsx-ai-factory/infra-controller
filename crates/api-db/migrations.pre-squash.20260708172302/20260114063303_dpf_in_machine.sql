-- Add migration script here
ALTER TABLE machines ADD COLUMN dpf_enabled BOOLEAN NOT NULL DEFAULT TRUE;
