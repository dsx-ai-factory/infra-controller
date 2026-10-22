-- no-transaction
-- The source-key index replaces the index over complete merged reports.
DROP INDEX CONCURRENTLY machine_health_reports_merges_gin_idx;
