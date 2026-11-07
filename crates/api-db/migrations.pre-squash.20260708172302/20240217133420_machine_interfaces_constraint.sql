ALTER TABLE machine_interfaces DROP CONSTRAINT one_primary_interface_per_machine;
CREATE UNIQUE INDEX one_primary_interface_per_machine ON machine_interfaces (machine_id) WHERE (primary_interface = true);
