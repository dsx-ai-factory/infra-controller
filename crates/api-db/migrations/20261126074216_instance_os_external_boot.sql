-- The DHCP option set that an instance with the External OS boots from.
ALTER TABLE instances
    ADD COLUMN os_external_boot_id uuid REFERENCES dhcp_option_sets(id);
