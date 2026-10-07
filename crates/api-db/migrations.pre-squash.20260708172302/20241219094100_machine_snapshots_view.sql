-- Views machine_snapshots and machine_snapshots_with_history are a denormalized views of the machines table, using
-- JSONB_AGG to bring in related data, including:
-- - machine_interfaces (itself a view which is denormalized)
-- - machine_topologies (the most recent topology)
CREATE OR REPLACE VIEW machine_snapshots AS
    WITH
    interfaces_agg AS (
        SELECT mi.machine_id, JSONB_AGG(mi.*) AS json
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
        SELECT mt.machine_id, JSONB_AGG(mt.*) AS json
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

-- View machine_snapshots_with_history takes the machine_snapshots view and adds in a "history" column of
-- machine_state_history
CREATE OR REPLACE VIEW machine_snapshots_with_history AS
    WITH
    history_agg AS (
        SELECT mh.machine_id, JSONB_AGG(mh.*) AS json
        FROM machine_state_history mh
        GROUP BY machine_id
    )
    SELECT
        m.*,
        COALESCE(history_agg.json, '[]') AS history
    FROM machine_snapshots m
    LEFT JOIN history_agg ON history_agg.machine_id = m.id;