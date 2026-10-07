-- Add migration script here
ALTER TABLE
  machines
ADD COLUMN discovery_machine_validation_id uuid,
ADD COLUMN cleanup_machine_validation_id uuid;

CREATE TABLE machine_validation (
  id uuid NOT NULL,
  machine_id VARCHAR(64) NOT NULL,
  start_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  name VARCHAR(64),
  end_time TIMESTAMPTZ,

  -- FIXME these contraints are failing. Some test cases fail for ON DELETE and som of ON UPDATE
  --        May be nede to update cleanup_machine_by_id procedure
  -- CONSTRAINT machine_id_delete_fk FOREIGN KEY (machine_id) REFERENCES machines(id) ON DELETE CASCADE,
  -- CONSTRAINT machine_id_update_fk FOREIGN KEY (machine_id) REFERENCES machines(id) ON UPDATE CASCADE
  PRIMARY KEY (id)
);

-- TODO work on text fields 
CREATE TABLE machine_validation_results (
  machine_validation_id uuid NOT NULL,
  name VARCHAR(64) NOT NULL,
  description VARCHAR(64),
  command text NOT NULL,
  args text,
  stdout text,
  stderr text,
  context VARCHAR(64),
  exit_code int DEFAULT 0,
  start_time TIMESTAMPTZ NOT NULL,
  end_time TIMESTAMPTZ NOT NULL,
  CONSTRAINT machine_validation_id_fk FOREIGN KEY (machine_validation_id) REFERENCES machine_validation(id) ON DELETE CASCADE
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
  delete from machines where id = deletion_machine_id;
  delete from machine_validation where machine_id = deletion_machine_id;
end
$$;
-- CREATE OR REPLACE FUNCTION machine_validation_keep_limit()
-- RETURNS TRIGGER AS
-- $body$
-- BEGIN
--   DELETE FROM
--     machine_validation
--   WHERE
--     machine_id NOT IN (
--       SELECT
--         machine_id, start_time
--       FROM
--         machine_validation
--       WHERE
--         machine_id = NEW.machine_id
--       ORDER BY
--         start_time DESC
--       LIMIT
--         15
--     );
--   RETURN NULL;
-- END;
-- $body$
-- LANGUAGE plpgsql;

-- CREATE TRIGGER t_machine_validation_keep_limit
-- AFTER
-- INSERT
--   ON machine_validation FOR EACH ROW EXECUTE PROCEDURE machine_validation_keep_limit();