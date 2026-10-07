ALTER TABLE IF EXISTS explored_endpoints
    ADD column last_redfish_powercycle TIMESTAMPTZ DEFAULT NULL;
