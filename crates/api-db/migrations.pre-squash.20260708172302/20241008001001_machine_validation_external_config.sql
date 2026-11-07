-- Add migration script here
ALTER TABLE
  machine_validation_external_config
ADD COLUMN version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268');
