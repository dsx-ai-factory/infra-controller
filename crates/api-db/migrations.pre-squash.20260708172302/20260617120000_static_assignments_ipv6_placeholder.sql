-- Add an IPv6 placeholder prefix to the static-assignments anchor segment.
-- The segment is Reserved, so the DHCP allocator never hands this address out;
-- it only lets external static IPv6 assignments sit on a dual-family segment.
INSERT INTO network_prefixes (segment_id, prefix, gateway, dhcpv6_link_address, num_reserved)
SELECT id, '100::/128'::cidr, NULL::inet, NULL::inet, 1
FROM network_segments
WHERE name = 'static-assignments'
ON CONFLICT DO NOTHING;
