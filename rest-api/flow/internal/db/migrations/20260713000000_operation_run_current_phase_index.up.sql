-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE operation_run
    ADD COLUMN current_phase_index INTEGER NOT NULL DEFAULT 0 CHECK (current_phase_index >= 0);

UPDATE operation_run AS orun
SET current_phase_index = COALESCE(
    (
        SELECT MIN(ort.phase_index)
        FROM operation_run_target AS ort
        WHERE ort.operation_run_id = orun.id
          AND ort.status NOT IN ('completed', 'failed', 'terminated', 'skipped')
    ),
    (
        SELECT MAX(ort.phase_index)
        FROM operation_run_target AS ort
        WHERE ort.operation_run_id = orun.id
    ),
    0
);
