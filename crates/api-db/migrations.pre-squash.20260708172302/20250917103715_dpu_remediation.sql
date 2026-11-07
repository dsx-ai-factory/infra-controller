CREATE TABLE dpu_remediations (
  id uuid DEFAULT gen_random_uuid() NOT NULL,
  metadata_description TEXT,
  metadata_name TEXT,
  metadata_labels JSONB NOT NULL DEFAULT ('{}'),
  script_author TEXT NOT NULL,
  script_reviewed_by TEXT,
  script TEXT NOT NULL,
  enabled BOOLEAN NOT NULL DEFAULT false, 
  retries INTEGER NOT NULL DEFAULT 0,
  creation_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (id)
);

CREATE TABLE applied_dpu_remediations (
  id uuid NOT NULL,
  dpu_machine_id VARCHAR(64) NOT NULL,
  attempt INTEGER NOT NULL,
  succeeded BOOLEAN NOT NULL,
  status JSONB NOT NULL DEFAULT ('{}'),
  applied_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  FOREIGN KEY (id) references dpu_remediations(id),
  FOREIGN KEY (dpu_machine_id) references machines(id),
  PRIMARY KEY (id, dpu_machine_id, attempt)
);

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
delete from machine_validation where machine_id = deletion_machine_id;
delete from dpa_interface_state_history where interface_id in (select id from dpa_interfaces where machine_id = deletion_machine_id);
delete from dpa_interfaces where machine_id = deletion_machine_id;
delete from applied_dpu_remediations where dpu_machine_id = deletion_machine_id;
delete from machines where id = deletion_machine_id;
end
$$;
