ALTER TABLE IF EXISTS explored_endpoints
    ADD column last_redfish_bmc_reset TIMESTAMPTZ DEFAULT NULL,
    ADD column last_ipmitool_bmc_reset TIMESTAMPTZ DEFAULT NULL,
    ADD column last_redfish_reboot TIMESTAMPTZ DEFAULT NULL;