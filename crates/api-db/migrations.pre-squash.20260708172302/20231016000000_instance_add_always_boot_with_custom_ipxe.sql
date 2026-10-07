-- Add the `always_boot_with_custom_ipxe` flag to instances
ALTER TABLE instances
    ADD COLUMN always_boot_with_custom_ipxe BOOLEAN DEFAULT FALSE
;
