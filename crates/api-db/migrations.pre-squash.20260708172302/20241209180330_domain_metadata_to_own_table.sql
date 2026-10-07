-- Add migration script here
ALTER TABLE domains DROP COLUMN IF EXISTS metadata;
CREATE TABLE domain_metadata (
    id SERIAL PRIMARY KEY,
    allow_axfr_from TEXT[] DEFAULT ARRAY[]::TEXT[]
);

ALTER TABLE domains ADD COLUMN domain_metadata_id INTEGER REFERENCES domain_metadata(id);
