-- Adds metadata related columns to the machines table
-- The name is set to the Machine ID of the Machine
-- This matches how Site-Explorer will now create the Machines
ALTER TABLE machines
    ADD COLUMN labels JSONB NOT NULL DEFAULT ('{}'),
    ADD COLUMN name VARCHAR(256) NOT NULL DEFAULT (''),
    ADD COLUMN description VARCHAR(1024) NOT NULL DEFAULT (''),
    ADD COLUMN version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268');
UPDATE machines set name = id;

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