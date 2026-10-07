ALTER TABLE expected_machines ADD COLUMN bmc_retain_credentials BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE expected_switches ADD COLUMN bmc_retain_credentials BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE expected_power_shelves ADD COLUMN bmc_retain_credentials BOOLEAN NOT NULL DEFAULT FALSE;
