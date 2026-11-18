-- Accept expected machine BMC usernames and passwords up to 255 characters.
ALTER TABLE expected_machines
    ALTER COLUMN bmc_password TYPE VARCHAR(255),
    ALTER COLUMN bmc_username TYPE VARCHAR(255);
