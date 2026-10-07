-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DROP INDEX task_trigger_idx;

ALTER TABLE task
    DROP CONSTRAINT task_trigger_complete,
    DROP COLUMN trigger_id,
    DROP COLUMN trigger_type;
