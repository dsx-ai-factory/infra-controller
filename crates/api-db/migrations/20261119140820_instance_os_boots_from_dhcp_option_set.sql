-- Marks instances whose OS boots from their DHCP option set's boot URL.
ALTER TABLE instances ADD COLUMN os_boots_from_dhcp_option_set boolean NOT NULL DEFAULT false;
