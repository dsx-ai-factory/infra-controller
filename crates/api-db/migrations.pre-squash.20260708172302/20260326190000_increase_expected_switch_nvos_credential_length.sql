-- Increase nvos_username and nvos_password column sizes from VARCHAR(16) to VARCHAR(64)
ALTER TABLE expected_switches
    ALTER COLUMN nvos_username TYPE VARCHAR(64),
    ALTER COLUMN nvos_password TYPE VARCHAR(64);
