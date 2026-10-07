--- 20240618185800_cleanup_measured_boot.sql
---
--- This migration updates the cleanup_machine_by_id stored procedure
--- to also clean up measured boot data. Since there is a foreign key
--- involved, doing a force-delete (which triggers this proc) fails,
--- since measured boot data is referencing it.
---
--- This also change the measurement reports constraint to the machines ID,
--- and not on machine_topologies; measured boot looks at topology data for
--- system profile info, so I just made it against that, but it can just as
--- well be on the machines table.

ALTER TABLE measurement_reports DROP CONSTRAINT measurement_reports_machine_id_fkey;
ALTER TABLE measurement_reports ADD CONSTRAINT measurement_reports_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES machines(id);

-- Cleans up a Machine by Machine ID
create or replace procedure cleanup_machine_by_id(deletion_machine_id varchar(64))
 language plpgsql as $$
 begin
  update machine_interfaces set machine_id = null where machine_id = deletion_machine_id;
  update machine_interfaces set attached_dpu_machine_id = null where attached_dpu_machine_id = deletion_machine_id;
  delete from measurement_journal where report_id in (select report_id from measurement_reports where machine_id = deletion_machine_id);
  delete from measurement_reports_values where report_id in (select report_id from measurement_reports where machine_id = deletion_machine_id);
  delete from measurement_reports where machine_id = deletion_machine_id;
  delete from measurement_approved_machines where machine_id = deletion_machine_id;
  delete from machine_topologies where machine_id = deletion_machine_id;
  delete from machines where id = deletion_machine_id;
end
$$;