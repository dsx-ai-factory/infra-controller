-- Allow stable /dev/disk/by-id paths whose aliases exceed 64 characters.
ALTER TABLE os_images
    ALTER COLUMN boot_disk TYPE text;
