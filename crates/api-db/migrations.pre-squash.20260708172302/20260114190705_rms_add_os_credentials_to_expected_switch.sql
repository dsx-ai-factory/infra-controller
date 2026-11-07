-- Add nvos_username and nvos_password columns to expected_switches table
-- These columns store NVOS credentials for switch management
ALTER TABLE
    expected_switches
ADD
    COLUMN nvos_username VARCHAR(16),
ADD
    COLUMN nvos_password VARCHAR(16);

