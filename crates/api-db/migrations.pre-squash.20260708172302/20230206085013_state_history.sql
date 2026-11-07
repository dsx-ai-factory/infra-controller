-- Add migration script here
DROP TABLE machine_events;

CREATE TABLE machine_state_history (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	machine_id uuid NOT NULL,
	state jsonb NOT NULL,
	state_version VARCHAR(64) NOT NULL,
	timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	FOREIGN KEY (machine_id) REFERENCES machines(id)
);

CREATE OR REPLACE FUNCTION machine_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
	DELETE FROM machine_state_history WHERE machine_id=NEW.machine_id AND id NOT IN (SELECT id from machine_state_history where machine_id=NEW.machine_id ORDER BY id DESC LIMIT 250);
	RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE TRIGGER t_machine_state_history_keep_limit
  AFTER INSERT ON machine_state_history
  FOR EACH ROW EXECUTE PROCEDURE machine_state_history_keep_limit();
