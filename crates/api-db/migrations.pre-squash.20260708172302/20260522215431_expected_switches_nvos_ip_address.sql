-- Add nvos_ip_address to expected_switches, mirroring bmc_ip_address.
-- Only meaningful when nvos_mac_addresses has exactly one entry (the single wired NVOS port).
ALTER TABLE expected_switches ADD COLUMN nvos_ip_address inet;
CREATE INDEX idx_expected_switches_nvos_ip_address ON expected_switches(nvos_ip_address);
