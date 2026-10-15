-- Let a machine-interface address outlive its interface as a reservation owned
-- by the interface MAC. An active row keeps interface_id; a parked row clears it
-- and records the owning MAC in reserved_by_mac. The existing global
-- UNIQUE (address) stays the one site-wide ownership check.
ALTER TABLE machine_interface_addresses
    ALTER COLUMN interface_id DROP NOT NULL,
    ADD COLUMN reserved_by_mac macaddr,
    ADD CONSTRAINT machine_interface_addresses_owner_check
        CHECK (interface_id IS NOT NULL OR reserved_by_mac IS NOT NULL);

-- At most one reservation per MAC and address family.
CREATE UNIQUE INDEX machine_interface_addresses_reserved_by_mac_family_key
    ON machine_interface_addresses (reserved_by_mac, family(address))
    WHERE reserved_by_mac IS NOT NULL;
