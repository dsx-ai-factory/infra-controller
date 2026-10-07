ALTER TABLE IF EXISTS machine_validation_external_config 
    DROP COLUMN config;

ALTER TABLE IF EXISTS machine_validation_external_config 
    ADD COLUMN config BYTEA NOT NULL;

