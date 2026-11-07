-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

ALTER TABLE operation_run
    ADD COLUMN total_phases INTEGER NOT NULL DEFAULT 0 CHECK (total_phases >= 0);

UPDATE operation_run AS orun
SET total_phases = phase_counts.total_phases
FROM (
    SELECT operation_run_id, MAX(phase_index) + 1 AS total_phases
    FROM operation_run_target
    GROUP BY operation_run_id
) AS phase_counts
WHERE phase_counts.operation_run_id = orun.id;

ALTER TABLE operation_run
    ALTER COLUMN total_phases DROP DEFAULT;
