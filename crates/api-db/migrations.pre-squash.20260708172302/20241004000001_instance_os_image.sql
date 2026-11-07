-- Add os image id to instances
ALTER TABLE IF EXISTS instances
    ADD COLUMN IF NOT EXISTS os_image_id uuid
;
