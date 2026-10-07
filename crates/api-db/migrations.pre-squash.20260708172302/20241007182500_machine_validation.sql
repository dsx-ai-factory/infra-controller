-- Add migration script here
ALTER TABLE
  machine_validation
ADD COLUMN filter JSONB,
ADD COLUMN context VARCHAR(64);