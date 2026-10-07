-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Persist event rules, their scoped bindings, deduplicated events, and the
-- immutable execution plans consumed by the event-rule scheduler.
CREATE TABLE event_rules (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    description text NOT NULL,
    enabled boolean NOT NULL,
    event_type text NOT NULL,
    policy jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT event_rules_name_check CHECK (
        name = btrim(name) AND name <> '' AND octet_length(name) <= 128
    ),
    CONSTRAINT event_rules_event_type_check CHECK (
        event_type ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$'
    ),
    CONSTRAINT event_rules_timestamps_check CHECK (updated_at >= created_at),
    CONSTRAINT event_rules_id_event_type_key UNIQUE (id, event_type)
);

CREATE INDEX event_rules_event_type_idx
    ON event_rules (event_type);

CREATE TABLE event_rule_bindings (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_id uuid NOT NULL,
    event_type text NOT NULL,
    scope_type text NOT NULL,
    scope_id uuid,
    created_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT event_rule_bindings_event_type_check CHECK (
        event_type ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$'
    ),
    CONSTRAINT event_rule_bindings_scope_check CHECK (
        (scope_type = 'site' AND scope_id IS NULL)
        OR (scope_type = 'rack' AND scope_id IS NOT NULL)
    ),
    CONSTRAINT event_rule_bindings_timestamps_check CHECK (
        updated_at >= created_at
    ),
    CONSTRAINT event_rule_bindings_rule_event_type_fkey FOREIGN KEY (
        rule_id,
        event_type
    ) REFERENCES event_rules (id, event_type) ON DELETE CASCADE
);

CREATE INDEX event_rule_bindings_rule_id_idx
    ON event_rule_bindings (rule_id);

CREATE INDEX event_rule_bindings_scope_idx
    ON event_rule_bindings (event_type, scope_type, scope_id);

CREATE UNIQUE INDEX event_rule_bindings_site_scope_uidx
    ON event_rule_bindings (event_type)
    WHERE scope_type = 'site';

CREATE UNIQUE INDEX event_rule_bindings_rack_scope_uidx
    ON event_rule_bindings (event_type, scope_id)
    WHERE scope_type = 'rack';

CREATE TABLE events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_name text NOT NULL,
    source_key text NOT NULL,
    event_type text NOT NULL,
    resource_id uuid NOT NULL,
    resource_kind text NOT NULL,
    applied_rule_id uuid NOT NULL,
    effective_policy jsonb NOT NULL,
    summary text NOT NULL,
    observations integer NOT NULL,
    created_at timestamp with time zone NOT NULL,
    last_observed_at timestamp with time zone NOT NULL,
    CONSTRAINT events_source_name_check CHECK (
        source_name ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT events_source_key_check CHECK (
        source_key = btrim(source_key) AND source_key <> ''
    ),
    CONSTRAINT events_event_type_check CHECK (
        event_type ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$'
    ),
    CONSTRAINT events_resource_kind_check CHECK (
        resource_kind IN ('rack', 'component')
    ),
    CONSTRAINT events_summary_check CHECK (
        summary = btrim(summary) AND char_length(summary) <= 1024
    ),
    CONSTRAINT events_observations_check CHECK (observations > 0),
    CONSTRAINT events_timestamps_check CHECK (last_observed_at >= created_at)
);

CREATE UNIQUE INDEX events_source_key_uidx
    ON events (source_name, source_key);

CREATE INDEX events_event_type_created_at_idx
    ON events (event_type, created_at, id);

CREATE TABLE event_action_executions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id uuid NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    action_name text NOT NULL,
    action_type text NOT NULL,
    plan jsonb NOT NULL,
    status text NOT NULL,
    reason text,
    attempts integer NOT NULL,
    claim_token uuid,
    claim_owner text,
    status_message text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    next_attempt_at timestamp with time zone,
    CONSTRAINT event_action_executions_action_name_check CHECK (
        action_name ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT event_action_executions_action_type_check CHECK (
        action_type IN ('submit_task', 'send_alert', 'noop')
    ),
    CONSTRAINT event_action_executions_status_check CHECK (
        status IN ('pending', 'running', 'skipped', 'deferred', 'completed', 'failed')
    ),
    CONSTRAINT event_action_executions_reason_check CHECK (
        (status IN ('pending', 'running', 'completed', 'failed') AND reason IS NULL)
        OR (status = 'skipped' AND reason = 'no_targets')
        OR (status = 'deferred' AND reason IN ('attempt_failed', 'attempt_interrupted'))
    ),
    CONSTRAINT event_action_executions_attempts_check CHECK (
        attempts >= 0
        AND (status NOT IN ('pending', 'skipped') OR attempts = 0)
        AND (
            status IN ('pending', 'skipped')
            OR attempts > 0
            OR (status = 'deferred' AND reason = 'attempt_interrupted')
        )
    ),
    CONSTRAINT event_action_executions_claim_check CHECK (
        (
            status = 'running'
            AND claim_token IS NOT NULL
            AND claim_owner IS NOT NULL
            AND claim_owner = btrim(claim_owner)
            AND claim_owner <> ''
            AND char_length(claim_owner) <= 128
        )
        OR (
            status <> 'running'
            AND claim_token IS NULL
            AND claim_owner IS NULL
        )
    ),
    CONSTRAINT event_action_executions_retry_check CHECK (
        (status = 'deferred' AND next_attempt_at IS NOT NULL)
        OR (status <> 'deferred' AND next_attempt_at IS NULL)
    ),
    CONSTRAINT event_action_executions_timestamps_check CHECK (
        updated_at >= created_at
    )
);

CREATE UNIQUE INDEX event_action_executions_event_action_uidx
    ON event_action_executions (event_id, action_name);

CREATE INDEX event_action_executions_pending_idx
    ON event_action_executions (created_at, id)
    WHERE status = 'pending';

CREATE INDEX event_action_executions_deferred_idx
    ON event_action_executions (next_attempt_at, id)
    WHERE status = 'deferred';

CREATE INDEX event_action_executions_event_status_idx
    ON event_action_executions (event_id, status);

CREATE INDEX event_action_executions_status_updated_at_idx
    ON event_action_executions (status, updated_at, id);

CREATE INDEX event_action_executions_action_type_idx
    ON event_action_executions (action_type);

CREATE INDEX event_action_executions_claim_owner_idx
    ON event_action_executions (claim_owner)
    WHERE status = 'running';
