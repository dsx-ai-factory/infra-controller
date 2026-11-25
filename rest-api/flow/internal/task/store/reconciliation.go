// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

func (s *PostgresStore) ListTasksForReconciliation(ctx context.Context, executor taskcommon.ExecutorType, afterID uuid.UUID, limit int) ([]*taskdef.Task, error) {
	if limit <= 0 {
		return nil, fmt.Errorf("reconciliation limit must be positive")
	}
	var rows []model.Task
	err := s.idb(ctx).NewSelect().Model(&rows).Column("id", "rack_id", "execution_id").
		Where("status IN (?)", bun.In([]taskcommon.TaskStatus{taskcommon.TaskStatusPending, taskcommon.TaskStatusRunning})).
		Where("executor_type = ?", executor).Where("execution_id <> ''").
		Where("id > ?", afterID).OrderExpr("id ASC").Limit(limit).Scan(ctx)
	if err != nil {
		return nil, fmt.Errorf("list tasks for reconciliation: %w", err)
	}
	tasks := make([]*taskdef.Task, 0, len(rows))
	for _, row := range rows {
		tasks = append(tasks, &taskdef.Task{ID: row.ID, RackID: row.RackID, ExecutionID: row.ExecutionID})
	}
	return tasks, nil
}

func (s *PostgresStore) FinishTaskExecution(ctx context.Context, task *taskdef.Task, status *taskdef.ExecutionStatus) (bool, error) {
	if task == nil || task.ExecutionID == "" || status == nil || !status.Status.IsFinished() || status.FinishedAt == nil {
		return false, fmt.Errorf("scheduled task and closed execution status are required")
	}
	res, err := s.idb(ctx).NewUpdate().Model((*model.Task)(nil)).
		// Reports remain the last observed activity snapshot; closing orchestration
		// cannot infer component outcomes or mark hardware work complete.
		Set("status = ?", status.Status).Set("message = ?", status.Message).
		Set("finished_at = ?", status.FinishedAt).Set("updated_at = ?", time.Now().UTC()).Set("queue_expires_at = NULL").
		Where("id = ?", task.ID).Where("execution_id = ?", task.ExecutionID).
		Where("status IN (?)", bun.In([]taskcommon.TaskStatus{taskcommon.TaskStatusPending, taskcommon.TaskStatusRunning})).Exec(ctx)
	if err != nil {
		return false, fmt.Errorf("finish task execution: %w", err)
	}
	count, err := res.RowsAffected()
	if err != nil {
		return false, fmt.Errorf("count finished tasks: %w", err)
	}
	return count == 1, nil
}
