-- Add bmc_ip_address to expected_switches (just like we do for power_shelves, etc).
ALTER TABLE expected_switches ADD COLUMN bmc_ip_address inet;
CREATE INDEX idx_expected_switches_bmc_ip_address ON expected_switches(bmc_ip_address);
