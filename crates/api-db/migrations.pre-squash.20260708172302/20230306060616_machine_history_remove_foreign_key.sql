-- Add migration script here
ALTER TABLE machine_state_history
 DROP CONSTRAINT machine_state_history_machine_id_fkey;

create or replace procedure cleanup_machine_by_id(deletion_machine_id uuid)
 language plpgsql as $$
 begin
  update machine_interfaces set machine_id = null, attached_dpu_machine_id = null where machine_id = deletion_machine_id;
  delete from machine_topologies where machine_id = deletion_machine_id;
  delete from machines where id = deletion_machine_id;
  delete from vpc_resource_leafs where id = deletion_machine_id;
end
$$;

