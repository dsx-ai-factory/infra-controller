-- Lets an instance point to the DHCP option set its host boots with.
ALTER TABLE instances
    ADD COLUMN dhcp_option_set_id uuid REFERENCES dhcp_option_sets(id) ON DELETE SET NULL;
