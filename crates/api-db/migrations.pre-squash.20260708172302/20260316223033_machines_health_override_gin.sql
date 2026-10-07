-- Add a column to create a generalized inverted index on machines table for health_report_overrides
CREATE INDEX IF NOT EXISTS machine_health_overrides_merges_gin_idx ON machines USING GIN ((health_report_overrides -> 'merges'));
