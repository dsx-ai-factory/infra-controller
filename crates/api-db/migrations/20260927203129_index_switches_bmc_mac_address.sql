-- Same lookup for switches; one index per migration, refer to
-- 20260927203128_index_machine_interfaces_mac_address.sql.
CREATE INDEX switches_bmc_mac_address_idx
    ON switches (bmc_mac_address);
