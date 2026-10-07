-- Table for NICs that connect to the Cluster Interconnect Network (i.e.
-- East West Ethernet)
CREATE TABLE IF NOT EXISTS dpa_interfaces (
    id                          uuid DEFAULT gen_random_uuid() NOT NULL,
    machine_id                  VARCHAR(64) NOT NULL,
    mac_address                 macaddr NOT NULL,
    created                     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated                     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted                     TIMESTAMPTZ,
    network_config              jsonb NOT NULL DEFAULT ('{}'),
    network_config_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    controller_state            jsonb NOT NULL DEFAULT ('{"state": "provisioning"}'),
    controller_state_version    VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    controller_state_outcome    jsonb,
    network_status_observation  jsonb NULL,

    PRIMARY KEY (id),
    FOREIGN KEY (machine_id) REFERENCES machines(id)
);

CREATE TABLE IF NOT EXISTS  dpa_interfaces_controller_lock(
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

CREATE TABLE IF NOT EXISTS  dpa_interface_state_history (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	interface_id uuid NOT NULL,
	state jsonb NOT NULL,
	state_version VARCHAR(64) NOT NULL,
	timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE OR REPLACE FUNCTION dpa_interface_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
	DELETE FROM dpa_interface_state_history WHERE interface_id=NEW.interface_id AND id NOT IN (SELECT id from dpa_interface_state_history where interface_id=NEW.interface_id ORDER BY id DESC LIMIT 250);
	RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER t_dpa_interface_state_history_keep_limit
  AFTER INSERT ON dpa_interface_state_history
  FOR EACH ROW EXECUTE PROCEDURE dpa_interface_state_history_keep_limit();

ALTER TABLE vpcs
  ADD column IF NOT EXISTS dpa_vni integer NULL UNIQUE;


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
  delete from machines where id = deletion_machine_id;
end
$$;
