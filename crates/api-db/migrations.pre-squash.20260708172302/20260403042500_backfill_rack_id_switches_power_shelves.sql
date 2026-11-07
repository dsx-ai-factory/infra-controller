-- Backfill rack_id in switches from expected_switches, joined on bmc_mac_address.
UPDATE switches s
SET    rack_id = es.rack_id
FROM   expected_switches es
WHERE  s.bmc_mac_address = es.bmc_mac_address
  AND  s.rack_id IS NULL
  AND  es.rack_id IS NOT NULL;

-- Backfill rack_id in power_shelves from expected_power_shelves, joined on serial_number.
UPDATE power_shelves ps
SET    rack_id = eps.rack_id
FROM   expected_power_shelves eps
WHERE  ps.config ->> 'name' = eps.serial_number
  AND  ps.rack_id IS NULL
  AND  eps.rack_id IS NOT NULL;
