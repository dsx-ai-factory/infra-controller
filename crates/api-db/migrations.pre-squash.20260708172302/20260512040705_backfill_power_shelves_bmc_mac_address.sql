-- Backfill power_shelves.bmc_mac_address, which we apparently had never
-- wired up to go from expected_power_shelves -> power_shelves.
UPDATE power_shelves ps
SET    bmc_mac_address = eps.bmc_mac_address
FROM   expected_power_shelves eps
WHERE  ps.config ->> 'name' = eps.serial_number
  AND  ps.bmc_mac_address IS NULL;
