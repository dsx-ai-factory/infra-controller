-- Seed expected_switch_nvos_interfaces from the existing nvos_mac_addresses
-- array, marking each switch's first MAC as primary. Ignore an unexpected
-- duplicate legacy MAC; the ordered input makes its winner deterministic.
INSERT INTO expected_switch_nvos_interfaces (mac_address, bmc_mac_address, is_primary)
SELECT port.mac_address, es.bmc_mac_address, port.ordinality = 1
FROM expected_switches es,
     LATERAL unnest(es.nvos_mac_addresses) WITH ORDINALITY AS port(mac_address, ordinality)
ORDER BY es.bmc_mac_address, port.ordinality
ON CONFLICT (mac_address) DO NOTHING;
