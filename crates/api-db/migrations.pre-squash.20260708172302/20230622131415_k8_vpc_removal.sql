-- The old Kubernetes VPC is gone. Remove it's database presence.

-- Remove the loopback_ip column from dpu_machines view
DROP VIEW dpu_machines;
CREATE VIEW dpu_machines AS (
    SELECT machines.id as machine_id,
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

-- Cleans up a Machine by Machine ID
create or replace procedure cleanup_machine_by_id(deletion_machine_id varchar(64))
 language plpgsql as $$
 begin
  update machine_interfaces set machine_id = null where machine_id = deletion_machine_id;
  update machine_interfaces set attached_dpu_machine_id = null where attached_dpu_machine_id = deletion_machine_id;
  delete from machine_topologies where machine_id = deletion_machine_id;
  delete from machines where id = deletion_machine_id;
end
$$;

ALTER TABLE machines DROP COLUMN vpc_leaf_id;
DROP TABLE vpc_resource_leafs;
