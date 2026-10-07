-- The cleanup_machine_by_id set `machine_id` and `attached_dpu_machine_id` to `null`
-- only for entries where the `machine_id` was defined. If a Host DHCPs but does not
-- run `forge-scout` it will however have `attached_dpu_machine_id` set but not
-- `machine_id`. Therefore we need to delete this field separately.


-- Cleans up a Machine by Machine ID
create or replace procedure cleanup_machine_by_id(deletion_machine_id varchar(64))
 language plpgsql as $$
 begin
  update machine_interfaces set machine_id = null where machine_id = deletion_machine_id;
  update machine_interfaces set attached_dpu_machine_id = null where attached_dpu_machine_id = deletion_machine_id;
  delete from machine_topologies where machine_id = deletion_machine_id;
  delete from machines where id = deletion_machine_id;
  delete from vpc_resource_leafs where id = deletion_machine_id;
end
$$;
