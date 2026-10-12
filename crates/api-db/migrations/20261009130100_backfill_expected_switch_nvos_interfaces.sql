-- Seed expected_switch_nvos_interfaces from the existing nvos_mac_addresses
-- array, marking each switch's first MAC as the primary port. A switch with a
-- single MAC therefore gets that MAC as primary; switches with no MACs get no
-- rows and stay grandfathered until their next create/update.
INSERT INTO expected_switch_nvos_interfaces (mac_address, bmc_mac_address, is_primary)
SELECT port.mac_address, es.bmc_mac_address, port.ordinality = 1
FROM expected_switches es,
     LATERAL unnest(es.nvos_mac_addresses) WITH ORDINALITY AS port(mac_address, ordinality);
