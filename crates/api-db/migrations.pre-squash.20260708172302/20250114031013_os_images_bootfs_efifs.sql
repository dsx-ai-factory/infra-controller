-- adds two new optional fields to os_images
-- bootfs_id and efifs_id, fields allow user to specify
-- UUIDs of the filesystem required to setup os image,
-- using qcow_imager
ALTER TABLE os_images
    ADD COLUMN bootfs_id VARCHAR(64),
    ADD COLUMN efifs_id VARCHAR(64);
