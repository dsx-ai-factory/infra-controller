-- Add bmc_mac_address to power_shelves, matching the pattern already used by
-- the switches table.  This gives a direct FK link from power_shelves to
-- expected_power_shelves and removes the need to join via config->>'name'.
ALTER TABLE power_shelves
ADD COLUMN bmc_mac_address macaddr REFERENCES expected_power_shelves(bmc_mac_address);

-- Backfill existing rows from expected_power_shelves via the legacy
-- serial_number / config->>'name' join.
UPDATE power_shelves ps
SET    bmc_mac_address = eps.bmc_mac_address
FROM   expected_power_shelves eps
WHERE  ps.config ->> 'name' = eps.serial_number
  AND  ps.bmc_mac_address IS NULL;
