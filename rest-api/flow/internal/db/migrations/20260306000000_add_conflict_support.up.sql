-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Add queue_expires_at for waiting tasks (null for all other statuses).
ALTER TABLE task ADD COLUMN IF NOT EXISTS queue_expires_at TIMESTAMPTZ;

-- Composite index to speed up active-task and waiting-task queries by rack.
CREATE INDEX IF NOT EXISTS idx_task_rack_status
    ON task(rack_id, status);
