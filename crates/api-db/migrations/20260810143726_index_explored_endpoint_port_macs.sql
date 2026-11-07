DROP INDEX explored_endpoints_mac_addresses_idx;

CREATE INDEX explored_endpoints_mac_addresses_idx
    ON explored_endpoints
    USING GIN (
        (
            jsonb_path_query_array(exploration_report,
                '$.Systems[*].EthernetInterfaces[*].MACAddress')
        ||
            jsonb_path_query_array(exploration_report,
                '$.Managers[*].EthernetInterfaces[*].MACAddress')
        ||
            jsonb_path_query_array(exploration_report,
                '$.Chassis[*].NetworkAdapters[*].PortMacAddresses[*]')
        ) jsonb_path_ops
    );
