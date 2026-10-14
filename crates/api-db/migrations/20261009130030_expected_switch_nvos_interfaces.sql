-- One row per switch NVOS ethernet port, with a flag marking the primary port
-- NICo talks to. Replaces the unordered expected_switches.nvos_mac_addresses
-- array as the source of truth for NVOS port selection. The partial unique index
-- allows at most one primary port per switch. Rust validates a non-empty set and
-- exactly one primary when an operator explicitly replaces the port set; legacy
-- zero-port switches remain valid until hardware discovery populates them.
CREATE TABLE expected_switch_nvos_interfaces (
    mac_address MACADDR PRIMARY KEY,
    bmc_mac_address MACADDR NOT NULL
        REFERENCES expected_switches(bmc_mac_address) ON UPDATE CASCADE ON DELETE CASCADE,
    is_primary BOOLEAN NOT NULL
);

CREATE UNIQUE INDEX expected_switch_nvos_interfaces_single_primary
    ON expected_switch_nvos_interfaces (bmc_mac_address)
    WHERE is_primary;
