-- This migration replaces many of the JSONB_AGG calls with JSON_AGG in the various snapshot views, since JSONB_AGG is
-- more expensive and has been shown to be slower. It also reads machine_state_history.state as TEXT instead of JSONB,
-- which is faster to return from the database.
--
-- Changes to the previous versions of the views are marked with MARK.
BEGIN;

DROP VIEW IF EXISTS machine_interface_snapshots CASCADE;
CREATE OR REPLACE VIEW machine_interface_snapshots AS
    WITH addresses_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG
        SELECT a.interface_id, JSON_AGG(a.address) AS json
        FROM machine_interface_addresses a
        GROUP BY a.interface_id
    ),
    vendors_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG
       SELECT d.machine_interface_id, JSON_AGG(d.vendor_string) AS json
       FROM dhcp_entries d
       GROUP BY d.machine_interface_id
    )
    SELECT mi.*,
        COALESCE(addresses_agg.json, '[]') AS addresses,
        COALESCE(vendors_agg.json, '[]') AS vendors
    FROM machine_interfaces mi
    LEFT JOIN addresses_agg ON (addresses_agg.interface_id = mi.id)
    LEFT JOIN vendors_agg ON (vendors_agg.machine_interface_id = mi.id)
    ORDER BY mi.created ASC;

DROP VIEW IF EXISTS machine_snapshots CASCADE;
CREATE OR REPLACE VIEW machine_snapshots AS
    WITH
    interfaces_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG
        SELECT mi.machine_id, JSON_AGG(mi.*) AS json
        FROM machine_interface_snapshots mi
        GROUP BY machine_id
    ),
    most_recent_topology AS (
        SELECT sub.machine_id, sub.topology, sub.created, sub.updated, sub.topology_update_needed
        FROM (
            SELECT mt.*, ROW_NUMBER()
            OVER (PARTITION BY mt.machine_id ORDER BY mt.created DESC) as row_num
            FROM machine_topologies mt
        ) sub
        WHERE row_num = 1
    ),
    topology_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG
        SELECT mt.machine_id, JSON_AGG(mt.*) AS json
        FROM most_recent_topology mt
        GROUP BY mt.machine_id
    )
    SELECT
        m.*,
        COALESCE(interfaces_agg.json, '[]') AS interfaces,
        COALESCE(topology_agg.json, '[]') AS topology
    FROM machines m
    LEFT JOIN interfaces_agg ON interfaces_agg.machine_id = m.id
    LEFT JOIN topology_agg ON topology_agg.machine_id = m.id;

DROP VIEW IF EXISTS machine_snapshots_with_history CASCADE;
CREATE OR REPLACE VIEW machine_snapshots_with_history AS
    WITH
    history_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG, fetch only the fields we want, and cast mh.state to TEXT
        SELECT mh.machine_id, JSON_AGG(json_build_object('machine_id', mh.machine_id, 'state', mh.state::TEXT, 'state_version', mh.state_version)) AS json
        FROM machine_state_history mh
        GROUP BY machine_id
    )
    SELECT
        m.*,
        COALESCE(history_agg.json, '[]') AS history
    FROM machine_snapshots m
    LEFT JOIN history_agg ON history_agg.machine_id = m.id;

DROP VIEW IF EXISTS network_segment_snapshots CASCADE;
CREATE OR REPLACE VIEW network_segment_snapshots AS
    WITH
    prefixes_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG
        SELECT np.segment_id, JSON_AGG(np.*) AS json
        FROM network_prefixes np
        GROUP BY np.segment_id
    )
    SELECT
        ns.*,
        COALESCE(prefixes_agg.json, '[]') AS prefixes
    FROM network_segments ns
    LEFT JOIN prefixes_agg ON prefixes_agg.segment_id = ns.id;

DROP VIEW IF EXISTS network_segment_snapshots_with_history CASCADE;
CREATE OR REPLACE VIEW network_segment_snapshots_with_history AS
    WITH
    history_agg AS (
        -- MARK: replace JSONB_AGG with JSON_AGG, fetch only the fields we want, and cast h.state to TEXT
        SELECT h.segment_id, JSON_AGG(json_build_object('segment_id', h.segment_id, 'state', h.state::TEXT, 'state_version', h.state_version, 'timestamp', h.timestamp)) AS json
        FROM network_segment_state_history h
        GROUP BY h.segment_id
    )
    SELECT
        ns.*,
        COALESCE(history_agg.json, '[]') AS history
    FROM network_segment_snapshots ns
    LEFT JOIN history_agg ON history_agg.segment_id = ns.id;

-- All done.
COMMIT;