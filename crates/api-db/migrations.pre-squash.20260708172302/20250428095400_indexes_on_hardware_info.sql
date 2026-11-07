-- Enables searching for MAC addresses in explored endpoints via an index
CREATE INDEX explored_endpoints_mac_addresses_idx
    ON explored_endpoints
    USING GIN (
        (
            jsonb_path_query_array(exploration_report,
                '$.Systems[*].EthernetInterfaces[*].MACAddress')
        ||
            jsonb_path_query_array(exploration_report,
                '$.Managers[*].EthernetInterfaces[*].MACAddress')
        ) jsonb_path_ops
    );

 -- Enables searching for serials in discovery data
CREATE INDEX machine_topologies_serial_numbers_idx
    ON machine_topologies
    USING GIN (
        (
            jsonb_path_query_array(topology,
                '$.discovery_data.Info.dmi_data.product_serial')
        ||
            jsonb_path_query_array(topology,
                '$.discovery_data.Info.dmi_data.board_serial')
        ||
            jsonb_path_query_array(topology,
                '$.discovery_data.Info.dmi_data.chassis_serial')
        ) jsonb_path_ops
    );