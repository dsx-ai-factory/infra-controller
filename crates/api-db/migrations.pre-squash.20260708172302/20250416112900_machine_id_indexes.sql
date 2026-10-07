CREATE INDEX IF NOT EXISTS machine_state_history_machine_id_idx ON machine_state_history (machine_id);
CREATE INDEX IF NOT EXISTS machine_topologies_machine_id_created_idx ON machine_topologies (machine_id, created DESC);
CREATE INDEX IF NOT EXISTS machine_interfaces_machine_id_idx ON machine_interfaces (machine_id);
CREATE INDEX IF NOT EXISTS dhcp_entries_mi_id_idx ON dhcp_entries (machine_interface_id);