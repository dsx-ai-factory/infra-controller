ALTER TABLE machines RENAME COLUMN health_report_overrides TO health_reports;
ALTER TABLE racks RENAME COLUMN health_report_overrides TO health_reports;
ALTER TABLE switches RENAME COLUMN health_report_overrides TO health_reports;
ALTER TABLE power_shelves RENAME COLUMN health_report_overrides TO health_reports;
ALTER INDEX machine_health_overrides_merges_gin_idx RENAME TO machine_health_reports_merges_gin_idx;
