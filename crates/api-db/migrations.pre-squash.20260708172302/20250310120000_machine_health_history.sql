CREATE TABLE machine_health_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    machine_id VARCHAR(256) NOT NULL,
    health jsonb NOT NULL,
    health_hash VARCHAR(32) NOT NULL,
    time TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_machine_health_history_machine_id ON machine_health_history (machine_id);

CREATE OR REPLACE FUNCTION machine_health_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM machine_health_history WHERE machine_id=NEW.machine_id AND id NOT IN (SELECT id from machine_health_history where machine_id=NEW.machine_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE TRIGGER t_machine_health_history_keep_limit
  AFTER INSERT ON machine_health_history
  FOR EACH ROW EXECUTE PROCEDURE machine_health_history_keep_limit();