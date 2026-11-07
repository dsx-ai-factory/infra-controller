-- Add migration script here
-- this is the default value set by SRE whether this machine should be paused before ingestion
ALTER TABLE expected_machines ADD COLUMN IF NOT EXISTS default_pause_ingestion_and_poweron boolean NOT NULL DEFAULT FALSE;

-- this value is set initially to that of expected_machines, but then can be changed by the carbide
ALTER TABLE explored_endpoints ADD COLUMN IF NOT EXISTS pause_ingestion_and_poweron boolean NOT NULL DEFAULT FALSE;