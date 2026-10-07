-- Index machine_interface_addresses by address. Reverse-DNS (PTR) resolution
-- parses a query name into an address and then looks the interface up by that
-- address, so this turns that lookup into an index probe rather than a scan.
CREATE INDEX machine_interface_addresses_address_idx
    ON machine_interface_addresses (address);
