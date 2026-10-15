-- The site explorer looks up machine_interfaces by MAC address once per
-- explored endpoint each run; the column had no leading index. One index per
-- migration, so the build never holds locks on two tables that nico-api
-- updates in one transaction.
CREATE INDEX machine_interfaces_mac_address_idx
    ON machine_interfaces (mac_address);
