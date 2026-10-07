-- View machine_interface_snapshots is a denormalized view of machine_interfaces, bringing in the addresses and vendor
-- vendor strings from related tables using JSONB_AGG.
CREATE OR REPLACE VIEW machine_interface_snapshots AS
    WITH addresses_agg AS (
        SELECT a.interface_id, JSONB_AGG(a.address) AS json
        FROM machine_interface_addresses a
        GROUP BY a.interface_id
    ),
    vendors_agg AS (
       SELECT d.machine_interface_id, JSONB_AGG(d.vendor_string) AS json
       FROM dhcp_entries d
       GROUP BY d.machine_interface_id
    )
    SELECT mi.*,
        COALESCE(addresses_agg.json, '[]') AS addresses,
        COALESCE(vendors_agg.json, '[]') AS vendors
    FROM machine_interfaces mi
    LEFT JOIN addresses_agg ON (addresses_agg.interface_id = mi.id)
    LEFT JOIN vendors_agg ON (vendors_agg.machine_interface_id = mi.id)
    ORDER BY mi.created ASC
