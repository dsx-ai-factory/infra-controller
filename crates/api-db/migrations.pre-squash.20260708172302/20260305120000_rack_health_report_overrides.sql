ALTER TABLE racks ADD COLUMN health_report_overrides jsonb;
CREATE INDEX idx_racks_on_compute_trays ON racks USING GIN ((config->'compute_trays'));
