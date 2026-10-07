-- Cleans up a Machine by machine_id
create procedure cleanup_machine_by_id(deletion_machine_id uuid)
language plpgsql as $$
begin
  update machine_interfaces set machine_id = null, attached_dpu_machine_id = null where machine_id = deletion_machine_id;
  delete from machine_topologies where machine_id = deletion_machine_id;
  delete from machine_state_history where machine_id = deletion_machine_id;
  delete from machines where id = deletion_machine_id;
  delete from vpc_resource_leafs where id = deletion_machine_id;
end
$$;

-- Cleans ups a Machine by hostname
create or replace procedure cleanup_machine(host varchar(63))
language plpgsql as $$
declare
  delete_id uuid;
begin
  select machine_id into delete_id from machine_interfaces where hostname = host;
  call cleanup_machine_by_id(delete_id);
end
$$;
