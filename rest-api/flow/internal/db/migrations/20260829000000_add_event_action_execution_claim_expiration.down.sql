-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE event_action_executions
    DROP CONSTRAINT event_action_executions_claim_expiration_check;

DROP INDEX event_action_executions_expired_claim_idx;

ALTER TABLE event_action_executions
    DROP COLUMN claim_expires_at;
