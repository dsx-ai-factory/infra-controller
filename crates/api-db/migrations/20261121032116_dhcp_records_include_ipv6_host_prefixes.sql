-- An infrastructure DHCPv6 lease can equal its /128 prefix. The
-- static-assignments segment has placeholder prefixes, not a DHCP subnet.
CREATE OR REPLACE VIEW machine_dhcp_records AS
SELECT
    m.id AS machine_id,
    mi.id AS machine_interface_id,
    ns.id AS segment_id,
    ns.subdomain_id,
    concat(mi.hostname, '.', d.name) AS fqdn,
    mi.mac_address,
    mia.address,
    ns.mtu,
    np.prefix,
    np.gateway,
    mid.last_deletion AS last_invalidation_time
FROM machine_interfaces mi
LEFT JOIN machines m ON m.id = mi.machine_id
JOIN network_segments ns ON ns.id = mi.segment_id
JOIN network_prefixes np ON np.segment_id = ns.id
JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
JOIN domains d ON d.id = mi.domain_id
JOIN machine_interfaces_deletion mid ON mid.id = 1
WHERE mia.address << np.prefix
   OR (family(mia.address) = 6 AND mia.address = np.prefix
       AND ns.network_segment_type IN ('admin', 'underlay', 'host_inband')
       AND ns.name <> 'static-assignments');
