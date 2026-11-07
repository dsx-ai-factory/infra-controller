-- Add a field to store the health report created by validation tests
ALTER TABLE machines
    ADD COLUMN machine_validation_health_report jsonb
    DEFAULT '{"source":"machine-validation","observed_at":"2024-09-16T12:00:00Z","successes":[],"alerts":[]}';
