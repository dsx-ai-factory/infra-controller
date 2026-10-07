-- Add a field to store the health report created by validation tests
ALTER TABLE machines
    ADD COLUMN site_explorer_health_report jsonb;
