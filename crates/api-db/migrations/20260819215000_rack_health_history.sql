-- Persist a deduplicated history of aggregate Rack health, mirroring
-- machine_health_history. Records are written by the rack controller each
-- iteration and deduplicated by health_hash; a row-level trigger keeps only the
-- most recent 250 rows per object_id. There is intentionally no foreign key to
-- racks(id) so history survives rack deletion and ID renames.
CREATE TABLE rack_health_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    object_id VARCHAR(256) NOT NULL,
    health jsonb NOT NULL,
    health_hash VARCHAR(32) NOT NULL,
    time TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_rack_health_history_object_id ON rack_health_history (object_id);

CREATE OR REPLACE FUNCTION rack_health_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM rack_health_history WHERE object_id=NEW.object_id AND id NOT IN (SELECT id from rack_health_history where object_id=NEW.object_id ORDER BY id DESC LIMIT 250);
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE TRIGGER t_rack_health_history_keep_limit
  AFTER INSERT ON rack_health_history
  FOR EACH ROW EXECUTE PROCEDURE rack_health_history_keep_limit();
