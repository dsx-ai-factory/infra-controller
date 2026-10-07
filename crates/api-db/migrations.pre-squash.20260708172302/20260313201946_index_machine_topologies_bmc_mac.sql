CREATE INDEX IF NOT EXISTS machine_topologies_bmc_mac_idx
    ON machine_topologies ((topology -> 'bmc_info' ->> 'mac'));
