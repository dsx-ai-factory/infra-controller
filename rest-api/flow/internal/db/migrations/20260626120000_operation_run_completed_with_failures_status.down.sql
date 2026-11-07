-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE operation_run
    DROP CONSTRAINT IF EXISTS operation_run_status_check;

UPDATE operation_run
SET status = 'failed'
WHERE status = 'completed_with_failures';

ALTER TABLE operation_run
    ADD CONSTRAINT operation_run_status_check CHECK (
        status IN (
            'pending',
            'running',
            'paused',
            'completed',
            'cancelled',
            'failed'
        )
    );

UPDATE operation_run_target
SET status = 'pending',
    message = NULL,
    retry_after = NULL,
    retry_state = NULL
WHERE status = 'claimed';

ALTER TABLE operation_run_target
    DROP CONSTRAINT IF EXISTS operation_run_target_status_check;

ALTER TABLE operation_run_target
    ADD CONSTRAINT operation_run_target_status_check CHECK (
        status IN (
            'pending',
            'blocked',
            'submitted',
            'completed',
            'failed',
            'terminated',
            'skipped'
        )
    );
