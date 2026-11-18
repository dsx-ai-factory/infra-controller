-- Match the REST BMC credential limits for expected switches.
ALTER TABLE expected_switches
    ALTER COLUMN bmc_username TYPE VARCHAR,
    ALTER COLUMN bmc_password TYPE VARCHAR(255);
