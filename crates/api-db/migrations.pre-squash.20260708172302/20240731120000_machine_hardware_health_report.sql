-- Add a field to store the health report sent by hardware health
ALTER TABLE machines ADD COLUMN hardware_health_report jsonb;
