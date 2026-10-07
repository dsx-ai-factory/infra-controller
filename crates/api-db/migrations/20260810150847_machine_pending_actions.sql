-- Durable record that an external system is waiting on carbide to act for a
-- machine, plus a short history of the actions already completed.
--
-- The state controller's work queue cannot carry this signal. It stores only an
-- object id and coalesces duplicate enqueues, so a handler waking up cannot tell
-- why it woke: a targeted wakeup and a periodic sweep are indistinguishable, and
-- the targeted one is silently dropped when the sweep queued the machine first.
-- Without a durable marker, every reconcile pass would have to re-derive the
-- answer from Kubernetes for every machine, every time.
--
-- An outstanding action is a row with a NULL `completed_at`; the consumer stamps
-- that column only once the work has actually succeeded. A machine that is not
-- currently eligible therefore keeps its outstanding row until it becomes
-- eligible, and completing the work leaves a record behind rather than erasing
-- the fact that it ran.
CREATE TYPE machine_pending_action_kind AS ENUM (
    'dpu_service_sync'
);

CREATE TABLE machine_pending_actions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    machine_id varchar(64) NOT NULL
        REFERENCES machines(id) ON UPDATE CASCADE ON DELETE CASCADE,
    kind machine_pending_action_kind NOT NULL,
    requested_at timestamptz NOT NULL DEFAULT statement_timestamp(),
    completed_at timestamptz
);

-- At most one outstanding request per machine and kind, so re-requesting an
-- action that is still owed updates that row instead of queueing a duplicate.
-- Completed rows are exempt, which is what lets them accumulate as history.
CREATE UNIQUE INDEX machine_pending_actions_outstanding_idx
    ON machine_pending_actions (machine_id, kind)
    WHERE completed_at IS NULL;

CREATE INDEX machine_pending_actions_machine_id_idx
    ON machine_pending_actions (machine_id);

CREATE FUNCTION machine_pending_actions_keep_limit() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- Writers for one machine share this lock so two completions cannot both
    -- trim from the same 20-row window.
    PERFORM pg_advisory_xact_lock(
        hashtextextended(NEW.machine_id, TG_RELID::bigint)
    );
    -- Only completed rows are trimmed. An outstanding row is the signal that
    -- work is still owed, and nothing re-creates it once dropped, so it must
    -- never be evicted by history retention.
    DELETE FROM machine_pending_actions
    WHERE machine_id = NEW.machine_id
      AND completed_at IS NOT NULL
      AND id NOT IN (
          SELECT id
          FROM machine_pending_actions
          WHERE machine_id = NEW.machine_id
            AND completed_at IS NOT NULL
          ORDER BY id DESC
          LIMIT 20
      );
    RETURN NULL;
END;
$$;

-- Completion is the only transition that grows the history, so an insert (which
-- always starts outstanding) cannot overflow the window.
CREATE TRIGGER t_machine_pending_actions_keep_limit
    AFTER UPDATE OF completed_at ON machine_pending_actions
    FOR EACH ROW
    WHEN (OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL)
    EXECUTE FUNCTION machine_pending_actions_keep_limit();
