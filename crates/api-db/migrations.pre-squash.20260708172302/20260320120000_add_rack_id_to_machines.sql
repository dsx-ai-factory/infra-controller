-- Add a rack_id column to machines
ALTER TABLE machines ADD COLUMN rack_id VARCHAR(64);

-- Backfill it from the RackIdentifier metadata if it exists
UPDATE machines SET rack_id=labels->>'RackIdentifier' WHERE rack_id IS NULL AND labels->>'RackIdentifier' is not NULL;
