-- removes hardware_health_report column from machines, replaced by health_override 
ALTER TABLE machines
DROP COLUMN hardware_health_report;
