-- There were two branches each defining machine_interface_snapshots, and the latter one effectively rolled back the
-- prior one. Redefine it to match 20250121090000_machine_interface_snapshots_add_network_segment_info.sql
CREATE OR REPLACE VIEW machine_interface_snapshots AS
    WITH addresses_agg AS (
        SELECT a.interface_id, JSON_AGG(a.address) AS json
        FROM machine_interface_addresses a
        GROUP BY a.interface_id
    ),
    vendors_agg AS (
       SELECT d.machine_interface_id, JSON_AGG(d.vendor_string) AS json
       FROM dhcp_entries d
       GROUP BY d.machine_interface_id
    )
SELECT mi.*,
       COALESCE(addresses_agg.json, '[]') AS addresses,
       COALESCE(vendors_agg.json, '[]') AS vendors,
       ns.network_segment_type AS network_segment_type
FROM machine_interfaces mi
         INNER JOIN network_segments ns ON ns.id = mi.segment_id
         LEFT JOIN addresses_agg ON (addresses_agg.interface_id = mi.id)
         LEFT JOIN vendors_agg ON (vendors_agg.machine_interface_id = mi.id)
ORDER BY mi.created ASC;