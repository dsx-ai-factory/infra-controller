ALTER TABLE machine_validation
ADD COLUMN last_heartbeat_at TIMESTAMPTZ;

CREATE INDEX machine_validation_active_heartbeat_idx
    ON machine_validation (last_heartbeat_at)
    WHERE end_time IS NULL AND state IN ('Started', 'InProgress');

CREATE INDEX machine_validation_run_items_active_heartbeat_idx
    ON machine_validation_run_items (last_heartbeat_at)
    WHERE state IN ('Pending', 'Running');

CREATE INDEX machine_validation_attempts_active_heartbeat_idx
    ON machine_validation_attempts (last_heartbeat_at)
    WHERE state IN ('Pending', 'Running');
