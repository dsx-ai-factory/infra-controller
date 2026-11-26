-- Lets a VPC point to the DHCP option set its hosts boot with.
ALTER TABLE vpcs
    ADD COLUMN dhcp_option_set_id uuid REFERENCES dhcp_option_sets(id) ON DELETE SET NULL;
