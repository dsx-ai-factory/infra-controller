-- Add migration script here
-- Add a column which can carry the stable machine ID
-- This might be temporary and could be removed later on
ALTER TABLE machines
    ADD COLUMN stable_id VARCHAR(64) NULL
;
