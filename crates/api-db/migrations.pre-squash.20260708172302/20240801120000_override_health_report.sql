-- Add a field to store override health reports
ALTER TABLE machines ADD COLUMN health_report_overrides jsonb;
