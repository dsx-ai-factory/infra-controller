DROP VIEW dpu_machines;
CREATE VIEW dpu_machines AS (
    SELECT machines.id as machine_id,
    machine_interfaces.id as machine_interfaces_id,
    machine_interfaces.mac_address as mac_address,
    machine_interface_addresses.address as address,
    machine_interfaces.hostname as hostname,
    machines.network_config->>'loopback_ip' as loopback_ip
    FROM machine_interfaces
    LEFT JOIN machines on machine_interfaces.machine_id=machines.id
    INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id=machine_interfaces.id
    WHERE machine_interfaces.attached_dpu_machine_id IS NOT NULL
    AND machine_interfaces.attached_dpu_machine_id = machine_interfaces.machine_id
);
-- We'd like to index on loopback_ip, but sqlx migrations will fail if we do that.
-- Probably because the field doesn't exist yet.
--CREATE UNIQUE INDEX ON ((machines.network_config->>'loopback_ip'));
