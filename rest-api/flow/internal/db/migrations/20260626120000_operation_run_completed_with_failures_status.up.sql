-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE operation_run
    DROP CONSTRAINT IF EXISTS operation_run_status_check;

ALTER TABLE operation_run
    ADD CONSTRAINT operation_run_status_check CHECK (
        status IN (
            'pending',
            'running',
            'paused',
            'completed',
            'completed_with_failures',
            'cancelled',
            'failed'
        )
    );

ALTER TABLE operation_run_target
    DROP CONSTRAINT IF EXISTS operation_run_target_status_check;

ALTER TABLE operation_run_target
    ADD CONSTRAINT operation_run_target_status_check CHECK (
        status IN (
            'pending',
            'claimed',
            'blocked',
            'submitted',
            'completed',
            'failed',
            'terminated',
            'skipped'
        )
    );
