-- Update the instance Operating System data model

-- We rename several columns in order to align them better with names in Forge cloud
-- We also utilize the metadata_config_version to version additional fields,
-- and therefore rename the colummn to instance_config_version

ALTER TABLE instances RENAME COLUMN custom_ipxe to os_ipxe_script;
ALTER TABLE instances RENAME COLUMN always_boot_with_custom_ipxe to os_always_boot_with_ipxe;
ALTER TABLE instances RENAME COLUMN user_data to os_user_data;
ALTER TABLE instances RENAME COLUMN phone_home_enabled to os_phone_home_enabled;
ALTER TABLE instances RENAME COLUMN metadata_version to config_version;
