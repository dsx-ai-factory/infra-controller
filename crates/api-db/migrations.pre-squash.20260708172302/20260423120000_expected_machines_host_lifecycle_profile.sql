ALTER TABLE expected_machines ADD COLUMN host_lifecycle_profile JSONB NOT NULL DEFAULT '{}';
ALTER TABLE machines ADD COLUMN host_profile JSONB NOT NULL DEFAULT '{"disable_lockdown": false}';
