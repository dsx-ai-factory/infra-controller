-- Add migration script here
CREATE VIEW host_machines AS (
    SELECT machines.id as machine_id,
    machine_interfaces.id as machine_interfaces_id,
    machine_interfaces.mac_address as mac_address,
    machine_interface_addresses.address as address,
    machine_interfaces.hostname as hostname
    FROM machine_interfaces
    LEFT JOIN machines on machine_interfaces.machine_id=machines.id
    INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id=machine_interfaces.id
    WHERE machine_interfaces.attached_dpu_machine_id IS NOT NULL
    AND machine_interfaces.machine_id IS NOT NULL
    AND machine_interfaces.attached_dpu_machine_id != machine_interfaces.machine_id
);
