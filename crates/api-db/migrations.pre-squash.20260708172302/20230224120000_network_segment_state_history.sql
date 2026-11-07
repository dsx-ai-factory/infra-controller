-- Add migration script here
CREATE TABLE network_segment_state_history (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	segment_id uuid NOT NULL,
	state jsonb NOT NULL,
	state_version VARCHAR(64) NOT NULL,
	timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE OR REPLACE FUNCTION network_segment_state_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
	DELETE FROM network_segment_state_history WHERE segment_id=NEW.segment_id AND id NOT IN (SELECT id from network_segment_state_history where segment_id=NEW.segment_id ORDER BY id DESC LIMIT 250);
	RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

CREATE TRIGGER t_network_segment_state_history_keep_limit
  AFTER INSERT ON network_segment_state_history
  FOR EACH ROW EXECUTE PROCEDURE network_segment_state_history_keep_limit();
