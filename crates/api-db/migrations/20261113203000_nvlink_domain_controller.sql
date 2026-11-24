-- Preserve existing declarations until their lifecycle is explicitly adopted.
ALTER TABLE nvlink_domains
    ADD COLUMN controller_state jsonb,
    ADD COLUMN controller_state_version varchar(64),
    ADD COLUMN controller_state_outcome jsonb,
    ADD COLUMN config jsonb NOT NULL DEFAULT '{}',
    ADD COLUMN version varchar(64),
    ADD COLUMN deleted timestamptz;

CREATE TABLE nvlink_domain_controller_iteration_ids (
    id bigserial PRIMARY KEY,
    started_at timestamptz DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE nvlink_domain_controller_queued_objects (
    object_id text PRIMARY KEY,
    processed_by text,
    processing_started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE nvlink_domain_state_history (
    id bigserial PRIMARY KEY,
    object_id text NOT NULL,
    state jsonb NOT NULL,
    state_version varchar(64) NOT NULL,
    timestamp timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX nvlink_domain_state_history_object_id_idx ON nvlink_domain_state_history(object_id);

CREATE FUNCTION nvlink_domain_state_history_keep_limit() RETURNS trigger
    LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.object_id, TG_RELID::bigint));
    DELETE FROM nvlink_domain_state_history
    WHERE object_id=NEW.object_id AND id NOT IN (
        SELECT id FROM nvlink_domain_state_history
        WHERE object_id=NEW.object_id ORDER BY id DESC LIMIT 250
    );
    RETURN NULL;
END;
$$;

CREATE TRIGGER t_nvlink_domain_state_history_keep_limit
AFTER INSERT ON nvlink_domain_state_history
FOR EACH ROW EXECUTE FUNCTION nvlink_domain_state_history_keep_limit();
