-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Identities and outcomes outlive machine/instance deletion for replay and recovery.
CREATE TABLE external_validation_holds (
    hold_id UUID PRIMARY KEY,
    machine_id VARCHAR(64) NOT NULL,
    tenant_org_id TEXT NOT NULL CHECK (tenant_org_id <> ''),
    validation_cycle_id UUID NOT NULL,
    health_report_source TEXT NOT NULL CHECK (health_report_source <> ''),
    state TEXT NOT NULL CHECK (state IN ('pending', 'attempt_open', 'awaiting_cleanup', 'satisfied', 'recovery')),
    created_at TIMESTAMPTZ NOT NULL,
    state_changed_at TIMESTAMPTZ NOT NULL,
    current_attempt_id UUID,
    recovery_reason TEXT,
    removed_at TIMESTAMPTZ,
    UNIQUE (machine_id, validation_cycle_id),
    UNIQUE (hold_id, tenant_org_id),
    CHECK ((state = 'recovery' AND recovery_reason IS NOT NULL AND recovery_reason <> '')
        OR (state <> 'recovery' AND recovery_reason IS NULL)),
    CHECK (state <> 'pending' OR current_attempt_id IS NULL),
    CHECK (state NOT IN ('attempt_open', 'awaiting_cleanup') OR current_attempt_id IS NOT NULL)
);
CREATE UNIQUE INDEX external_validation_one_active_hold
    ON external_validation_holds (machine_id) WHERE state <> 'satisfied' AND removed_at IS NULL;
CREATE INDEX external_validation_hold_discovery
    ON external_validation_holds (created_at, hold_id);

CREATE TABLE external_validation_attempts (
    request_id UUID PRIMARY KEY,
    hold_id UUID NOT NULL,
    tenant_org_id TEXT NOT NULL,
    caller_idempotency_key UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'passed', 'failed', 'cancelled')),
    allocation_state TEXT NOT NULL CHECK (allocation_state IN ('not_started', 'creating', 'created')),
    validation_instance_id UUID,
    opened_at TIMESTAMPTZ NOT NULL,
    details TEXT CHECK (octet_length(details) <= 4096),
    closed_at TIMESTAMPTZ,
    closure_kind TEXT CHECK (closure_kind IN ('cancel', 'complete')),
    UNIQUE (tenant_org_id, caller_idempotency_key),
    UNIQUE (request_id, hold_id),
    FOREIGN KEY (hold_id, tenant_org_id) REFERENCES external_validation_holds (hold_id, tenant_org_id),
    CHECK ((state = 'open' AND closed_at IS NULL AND closure_kind IS NULL AND details IS NULL)
        OR (state <> 'open' AND closed_at IS NOT NULL AND closure_kind IS NOT NULL)),
    CHECK ((allocation_state = 'created') = (validation_instance_id IS NOT NULL)),
    CHECK (closure_kind IS DISTINCT FROM 'cancel'
        OR (state = 'cancelled' AND allocation_state = 'not_started' AND details IS NOT NULL AND details <> '')),
    CHECK (closure_kind IS DISTINCT FROM 'complete' OR allocation_state = 'created')
);
CREATE UNIQUE INDEX external_validation_one_open_attempt
    ON external_validation_attempts (hold_id) WHERE state = 'open';
ALTER TABLE external_validation_holds ADD CONSTRAINT external_validation_current_attempt
    FOREIGN KEY (current_attempt_id, hold_id) REFERENCES external_validation_attempts (request_id, hold_id);

CREATE TABLE external_validation_audit (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_id UUID NOT NULL REFERENCES external_validation_attempts (request_id),
    action TEXT NOT NULL CHECK (action IN ('start', 'cancel', 'complete')),
    actor TEXT NOT NULL CHECK (actor <> ''),
    recorded_at TIMESTAMPTZ NOT NULL
);
