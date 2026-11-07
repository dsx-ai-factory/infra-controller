-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

DROP TRIGGER IF EXISTS operation_run_target_set_updated_at ON operation_run_target;
DROP INDEX IF EXISTS idx_operation_run_target_task;
DROP INDEX IF EXISTS idx_operation_run_target_run_status_retry_after;
DROP INDEX IF EXISTS idx_operation_run_target_run_phase_sequence;
DROP INDEX IF EXISTS idx_operation_run_target_run_status;
DROP TABLE IF EXISTS operation_run_target;

DROP TRIGGER IF EXISTS operation_run_set_updated_at ON operation_run;
DROP INDEX IF EXISTS idx_operation_run_created_at;
DROP INDEX IF EXISTS idx_operation_run_operation;
DROP INDEX IF EXISTS idx_operation_run_status_updated_at;
DROP TABLE IF EXISTS operation_run;
