-- Adds metadata related columns to the expected_machines table
-- It will carry the default metadata that will be applied to newly created Machines

ALTER TABLE expected_machines
    ADD COLUMN metadata_name VARCHAR(256) NOT NULL DEFAULT (''),
    ADD COLUMN metadata_description VARCHAR(1024) NOT NULL DEFAULT (''),
    ADD COLUMN metadata_labels JSONB NOT NULL DEFAULT ('{}');
