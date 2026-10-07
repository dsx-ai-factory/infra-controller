ALTER TABLE expected_machines
    ALTER COLUMN bmc_password TYPE VARCHAR(20),
    ALTER COLUMN bmc_password SET NOT NULL;