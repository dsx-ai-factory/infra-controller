-- Lets instances and VPCs point to the DHCP option set their hosts boot with.
ALTER TABLE instances
    ADD COLUMN dhcp_option_set_id uuid REFERENCES dhcp_option_sets(id) ON DELETE SET NULL;
ALTER TABLE vpcs
    ADD COLUMN dhcp_option_set_id uuid REFERENCES dhcp_option_sets(id) ON DELETE SET NULL;
