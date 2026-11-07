-- Remove the field log_parser_health_report which is replaced by health overrides
ALTER TABLE machines
DROP COLUMN log_parser_health_report;
