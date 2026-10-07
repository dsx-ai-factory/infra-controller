---
--- 20241127155004_block_storage_os_image_status.sql
--- This update status type to enum

UPDATE 
    os_images 
SET 
    status = 'uninitialized' 
WHERE 
    status = '' OR status NOT IN ('uninitialized', 'inprogress', 'failed', 'ready', 'disabled');

CREATE TYPE os_image_status AS ENUM ('uninitialized', 'inprogress', 'failed', 'ready', 'disabled');
ALTER TABLE os_images
    ALTER COLUMN status TYPE os_image_status USING status::text::os_image_status,
    ALTER COLUMN status SET NOT NULL,
    ALTER COLUMN status SET DEFAULT 'uninitialized';