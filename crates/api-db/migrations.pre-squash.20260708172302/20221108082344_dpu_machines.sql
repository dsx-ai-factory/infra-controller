-- Add migration script here
-- Returns all DPU machines. A DPU machine will have both attached_dpu_machine_id and machine_id same.
CREATE OR REPLACE VIEW dpu_machines AS (
    SELECT machines.id as machine_id,
    machines.vpc_leaf_id as vpc_leaf_id,
    machine_interfaces.id as machine_interfaces_id,
    machine_interfaces.mac_address as mac_address,
    machine_interface_addresses.address as address,
    machine_interfaces.hostname as hostname
    FROM machine_interfaces
    LEFT JOIN machines on machine_interfaces.machine_id=machines.id
    INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id=machine_interfaces.id
    WHERE machine_interfaces.attached_dpu_machine_id IS NOT NULL 
    AND machine_interfaces.attached_dpu_machine_id = machine_interfaces.machine_id
);
