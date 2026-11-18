-- no-transaction
-- Index report source names without storing the nested health-report payloads.
CREATE INDEX CONCURRENTLY machine_health_source_keys_gin_idx ON machines
USING gin (jsonb_path_query_array(health_reports, '$.merges.keyvalue().key'::jsonpath));
