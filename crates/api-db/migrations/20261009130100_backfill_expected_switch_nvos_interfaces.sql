-- Seed expected_switch_nvos_interfaces from the existing nvos_mac_addresses
-- array, marking each switch's first MAC as the primary port. A switch with a
-- single MAC gets that MAC as primary; switches with no MACs get no rows and
-- stay grandfathered until their next create/update.
--
-- Legacy nvos_mac_addresses are not unique: site-explorer hardware-truth writes
-- were unguarded, so the same NVOS MAC can appear on two switches or twice in
-- one array, while mac_address is this table's primary key. Insert ordered by
-- BMC MAC then array position so the lowest BMC MAC (and each switch's first
-- port) wins, then drop later colliding rows with ON CONFLICT so the backfill
-- cannot abort on the primary-key collision. Few sites have expected switches
-- populated today, so the rare cross-switch collision -- where the losing
-- switch keeps only its remaining ports -- is an accepted risk.
INSERT INTO expected_switch_nvos_interfaces (mac_address, bmc_mac_address, is_primary)
SELECT port.mac_address, es.bmc_mac_address, port.ordinality = 1
FROM expected_switches es,
     LATERAL unnest(es.nvos_mac_addresses) WITH ORDINALITY AS port(mac_address, ordinality)
ORDER BY es.bmc_mac_address, port.ordinality
ON CONFLICT (mac_address) DO NOTHING;
