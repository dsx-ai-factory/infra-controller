-- DHCPv6 relay link-address that identifies a segment. Unlike `gateway`,
-- it is not required to fall within the prefix, so it gets its own column.
ALTER TABLE network_prefixes
    ADD COLUMN dhcpv6_link_address INET
        CONSTRAINT network_prefix_dhcpv6_link_address_ipv6
            CHECK (dhcpv6_link_address IS NULL OR family(dhcpv6_link_address) = 6)
        CONSTRAINT network_prefix_dhcpv6_link_address_host
            CHECK (dhcpv6_link_address IS NULL OR masklen(dhcpv6_link_address) = 128);

-- Disambiguate equality matches: at most one prefix may claim a given
-- link-address.
CREATE UNIQUE INDEX network_prefix_dhcpv6_link_address
    ON network_prefixes (dhcpv6_link_address)
    WHERE dhcpv6_link_address IS NOT NULL;
