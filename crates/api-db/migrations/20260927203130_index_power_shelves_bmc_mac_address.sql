-- Same lookup for power shelves; one index per migration, refer to
-- 20260927203128_index_machine_interfaces_mac_address.sql.
CREATE INDEX power_shelves_bmc_mac_address_idx
    ON power_shelves (bmc_mac_address);
