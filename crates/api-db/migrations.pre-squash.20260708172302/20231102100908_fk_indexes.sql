-- Create missing indexes on foreign keys

CREATE INDEX IF NOT EXISTS network_segments_subdomain_id_idx ON network_segments (subdomain_id);
CREATE INDEX IF NOT EXISTS port_to_network_device_map_network_device_id_idx ON port_to_network_device_map (network_device_id);
CREATE INDEX IF NOT EXISTS network_segments_vpc_id_idx ON network_segments (vpc_id);
CREATE INDEX IF NOT EXISTS machine_interfaces_attached_dpu_machine_id_idx ON machine_interfaces (attached_dpu_machine_id);
CREATE INDEX IF NOT EXISTS network_prefixes_segment_id_idx ON network_prefixes (segment_id);
CREATE INDEX IF NOT EXISTS bmc_machine_machine_interface_id_idx ON bmc_machine (machine_interface_id);
