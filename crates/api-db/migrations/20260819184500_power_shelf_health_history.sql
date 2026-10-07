-- Persist a deduplicated history of aggregate Power Shelf health, mirroring
-- machine_health_history. Records are written by the power shelf controller each
-- iteration and deduplicated by health_hash; a row-level trigger keeps only the
-- most recent 250 rows per object_id. There is intentionally no foreign key to
-- power_shelves(id) so history survives power shelf deletion and ID renames.
CREATE TABLE power_shelf_health_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    object_id VARCHAR(256) NOT NULL,
    health jsonb NOT NULL,
    health_hash VARCHAR(32) NOT NULL,
    time TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_power_shelf_health_history_object_id ON power_shelf_health_history (object_id);

CREATE OR REPLACE FUNCTION power_shelf_health_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM power_shelf_health_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id from power_shelf_health_history where object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE TRIGGER t_power_shelf_health_history_keep_limit
  AFTER INSERT ON power_shelf_health_history
  FOR EACH ROW EXECUTE PROCEDURE power_shelf_health_history_keep_limit();
