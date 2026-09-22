-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Persist the lease deadline used to reclaim abandoned action executions.
ALTER TABLE event_action_executions
    ADD COLUMN claim_expires_at timestamp with time zone;

ALTER TABLE event_action_executions
    ADD CONSTRAINT event_action_executions_claim_expiration_check
        CHECK (
            (
                status = 'running'
                AND claim_expires_at IS NOT NULL
                AND claim_expires_at > updated_at
            )
            OR (
                status <> 'running'
                AND claim_expires_at IS NULL
            )
        ) NOT VALID;

CREATE INDEX event_action_executions_expired_claim_idx
    ON event_action_executions (claim_expires_at, id)
    WHERE status = 'running';
