-- Rename ip_address to bmc_ip_address for naming consistency.
ALTER TABLE expected_power_shelves RENAME COLUMN ip_address TO bmc_ip_address;
ALTER INDEX idx_expected_power_shelves_ip_address RENAME TO idx_expected_power_shelves_bmc_ip_address;
