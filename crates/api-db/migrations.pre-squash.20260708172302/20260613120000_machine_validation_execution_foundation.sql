CREATE TABLE machine_validation_run_items (
    id uuid NOT NULL,
    run_id uuid NOT NULL,
    test_id TEXT NOT NULL,
    test_version TEXT,
    display_name TEXT NOT NULL,
    context TEXT NOT NULL,
    component TEXT,
    state TEXT NOT NULL DEFAULT 'Pending',
    order_index INTEGER NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 1,
    timeout_seconds BIGINT NOT NULL DEFAULT 7200,
    started_at TIMESTAMPTZ,
    ended_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ,
    skip_reason TEXT,
    failure_reason TEXT,
    PRIMARY KEY (id),
    CONSTRAINT machine_validation_run_items_run_id_fk
        FOREIGN KEY (run_id) REFERENCES machine_validation(id) ON DELETE CASCADE,
    CONSTRAINT machine_validation_run_items_state_check
        CHECK (state IN ('Pending', 'Running', 'Success', 'Skipped', 'Failed')),
    CONSTRAINT machine_validation_run_items_attempt_check
        CHECK (attempt >= 0),
    CONSTRAINT machine_validation_run_items_max_attempts_check
        CHECK (max_attempts > 0),
    CONSTRAINT machine_validation_run_items_order_check
        CHECK (order_index >= 0),
    CONSTRAINT machine_validation_run_items_timeout_check
        CHECK (timeout_seconds >= 0)
);

CREATE UNIQUE INDEX machine_validation_run_items_run_test_idx
    ON machine_validation_run_items (run_id, test_id);

CREATE INDEX machine_validation_run_items_run_order_idx
    ON machine_validation_run_items (run_id, order_index);

CREATE TABLE machine_validation_attempts (
    id uuid NOT NULL,
    run_item_id uuid NOT NULL,
    attempt_number INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'Pending',
    command TEXT,
    args TEXT,
    container_image TEXT,
    execute_in_host BOOLEAN,
    exit_code INTEGER,
    failure_classification TEXT,
    started_at TIMESTAMPTZ,
    ended_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ,
    stdout_summary TEXT,
    stderr_summary TEXT,
    PRIMARY KEY (id),
    CONSTRAINT machine_validation_attempts_run_item_id_fk
        FOREIGN KEY (run_item_id) REFERENCES machine_validation_run_items(id) ON DELETE CASCADE,
    CONSTRAINT machine_validation_attempts_state_check
        CHECK (state IN ('Pending', 'Running', 'Success', 'Skipped', 'Failed')),
    CONSTRAINT machine_validation_attempts_attempt_number_check
        CHECK (attempt_number > 0)
);

CREATE UNIQUE INDEX machine_validation_attempts_item_attempt_idx
    ON machine_validation_attempts (run_item_id, attempt_number);

CREATE INDEX machine_validation_attempts_run_item_idx
    ON machine_validation_attempts (run_item_id);
