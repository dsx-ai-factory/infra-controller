-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

CREATE TABLE task_schedule (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                VARCHAR(255) NOT NULL UNIQUE,
    spec_type           VARCHAR(16) NOT NULL CHECK (spec_type IN ('interval', 'cron', 'one-time')),
    spec                TEXT NOT NULL,          -- duration string, cron expression, or RFC3339 timestamp
    timezone            VARCHAR(64) NOT NULL DEFAULT 'UTC',
    operation_template  JSONB NOT NULL,         -- serialized operation type + parameters (no target)
    overlap_policy      VARCHAR(16) NOT NULL DEFAULT 'skip' CHECK (overlap_policy IN ('skip', 'queue')),
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    next_run_at         TIMESTAMPTZ,
    last_run_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT current_timestamp
);

CREATE INDEX idx_task_schedule_next_run ON task_schedule (next_run_at)
    WHERE enabled = TRUE AND next_run_at IS NOT NULL;

CREATE TRIGGER task_schedule_set_updated_at
    BEFORE UPDATE ON task_schedule
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE task_schedule_scope (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    schedule_id     UUID NOT NULL REFERENCES task_schedule(id) ON DELETE CASCADE,
    rack_id         UUID NOT NULL REFERENCES rack(id) ON DELETE CASCADE,
    component_filter JSONB,          -- NULL = all components; {kind:"types",...} or {kind:"components",...}
    last_task_id    UUID REFERENCES task(id) ON DELETE SET NULL,  -- task ID from the most recent firing for this rack
    created_at      TIMESTAMPTZ NOT NULL DEFAULT current_timestamp,
    UNIQUE (schedule_id, rack_id)
);

CREATE INDEX idx_task_schedule_scope_rack ON task_schedule_scope (rack_id);
