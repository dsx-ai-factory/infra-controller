-- View network_segment_snapshots is a denormalized view of network_segments, bringing in the related network_prefixes
-- data through JSONB_AGG.
CREATE OR REPLACE VIEW network_segment_snapshots AS
    WITH
    prefixes_agg AS (
        SELECT np.segment_id, JSONB_AGG(np.*) AS json
        FROM network_prefixes np
        GROUP BY np.segment_id
    )
    SELECT
        ns.*,
        COALESCE(prefixes_agg.json, '[]') AS prefixes
    FROM network_segments ns
    LEFT JOIN prefixes_agg ON prefixes_agg.segment_id = ns.id;

-- View network_segment_snapshots_with_history takes the network_segment_snapshots view and adds in a "history" column
-- of network_segment_state_history
CREATE OR REPLACE VIEW network_segment_snapshots_with_history AS
    WITH
    history_agg AS (
        SELECT h.segment_id, JSONB_AGG(h.*) AS json
        FROM network_segment_state_history h
        GROUP BY h.segment_id
    )
    SELECT
        ns.*,
        COALESCE(history_agg.json, '[]') AS history
    FROM network_segment_snapshots ns
    LEFT JOIN history_agg ON history_agg.segment_id = ns.id;