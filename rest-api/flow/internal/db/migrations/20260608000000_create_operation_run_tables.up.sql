-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

CREATE TABLE operation_run (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                  VARCHAR(255) NOT NULL,
    description           TEXT,
    status                VARCHAR(32) NOT NULL DEFAULT 'pending' CHECK (
        status IN (
            'pending',
            'running',
            'paused',
            'completed',
            'cancelled',
            'failed'
        )
    ),
    status_reason         VARCHAR(64) NOT NULL DEFAULT 'none' CHECK (
        status_reason IN (
            'none',
            'operator_paused',
            'phase_gate',
            'safety_gate',
            'conflict_retry_timeout'
        )
    ),
    status_message        TEXT,
    selector              JSONB NOT NULL,
    options               JSONB NOT NULL,
    operation_template    JSONB NOT NULL,
    operation_type        VARCHAR(64) NOT NULL,
    operation_code        VARCHAR(64) NOT NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
    started_at            TIMESTAMPTZ,
    finished_at           TIMESTAMPTZ
);

CREATE INDEX idx_operation_run_status_updated_at
    ON operation_run (status, updated_at);

CREATE INDEX idx_operation_run_operation
    ON operation_run (operation_type, operation_code);

CREATE INDEX idx_operation_run_created_at
    ON operation_run (created_at);

CREATE TRIGGER operation_run_set_updated_at
    BEFORE UPDATE ON operation_run
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE operation_run_target (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    operation_run_id      UUID NOT NULL REFERENCES operation_run(id) ON DELETE CASCADE,
    rack_id               UUID NOT NULL,
    sequence_index        INTEGER NOT NULL CHECK (sequence_index >= 0),
    phase_index           INTEGER NOT NULL CHECK (phase_index >= 0),
    component_filter      JSONB,
    task_id               UUID REFERENCES task(id) ON DELETE SET NULL,
    status                VARCHAR(32) NOT NULL DEFAULT 'pending' CHECK (
        status IN (
            'pending',
            'blocked',
            'submitted',
            'completed',
            'failed',
            'terminated',
            'skipped'
        )
    ),
    message               TEXT,
    retry_after           TIMESTAMPTZ,
    retry_state           JSONB,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
    UNIQUE (operation_run_id, rack_id),
    UNIQUE (operation_run_id, sequence_index)
);

CREATE INDEX idx_operation_run_target_run_status
    ON operation_run_target (operation_run_id, status);

CREATE INDEX idx_operation_run_target_run_phase_sequence
    ON operation_run_target (operation_run_id, phase_index, sequence_index);

CREATE INDEX idx_operation_run_target_run_status_retry_after
    ON operation_run_target (operation_run_id, status, retry_after);

CREATE INDEX idx_operation_run_target_task
    ON operation_run_target (task_id)
    WHERE task_id IS NOT NULL;

CREATE TRIGGER operation_run_target_set_updated_at
    BEFORE UPDATE ON operation_run_target
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
