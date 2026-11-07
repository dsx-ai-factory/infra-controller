-- Rename the machine_id field in the machine health history table to object_id
-- in order to make the access logic compatible with other health history tables

DROP INDEX idx_machine_health_history_machine_id;
ALTER TABLE machine_health_history RENAME COLUMN machine_id TO object_id;
CREATE INDEX idx_machine_health_history_object_id ON machine_health_history (object_id);

CREATE OR REPLACE FUNCTION machine_health_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM machine_health_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id from machine_health_history where object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;
