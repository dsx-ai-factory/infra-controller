create or replace procedure cleanup_machine(host varchar(63))
language plpgsql as $$
declare
  delete_id uuid;
begin
  select machine_id into delete_id from machine_interfaces where hostname = host;
  update machine_interfaces set machine_id = null, attached_dpu_machine_id = null where hostname = host;
  delete from machine_topologies where machine_id = delete_id;
  delete from machine_events where machine_id  = delete_id;
  delete from machines where id = delete_id;
  delete from vpc_resource_leafs where id = delete_id;
end
$$;
