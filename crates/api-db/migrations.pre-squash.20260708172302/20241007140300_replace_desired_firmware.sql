-- We regenerate this table when carbide-api restarts, so it's okay that we lose the old data.
ALTER TABLE desired_firmware
    DROP COLUMN versions,
    ADD COLUMN versions JSONB NOT NULL DEFAULT '{}';
