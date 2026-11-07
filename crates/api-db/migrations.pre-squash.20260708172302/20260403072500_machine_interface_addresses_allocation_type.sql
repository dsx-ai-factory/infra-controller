-- Add allocation_type to distinguish DHCP-allocated addresses from
-- statically assigned ones. This allows for coexistence and cooperation
-- between DHCP-managed and static allocations.
ALTER TABLE machine_interface_addresses
    ADD COLUMN allocation_type TEXT NOT NULL DEFAULT 'dhcp';
