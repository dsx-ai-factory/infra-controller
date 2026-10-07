// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// FetchRunnableIDs returns up to limit pending/running operation runs for
// dispatcher work. limit must be positive; callers that want defaults should
// apply them before reaching the store. The returned IDs are not locked;
// LockRunnable owns the row lock.
func (s *PostgresStore) FetchRunnableIDs(
	ctx context.Context,
	limit int,
) ([]uuid.UUID, error) {
	if limit <= 0 {
		return nil, fmt.Errorf("fetch runnable operation run limit must be greater than 0")
	}

	var ids []uuid.UUID
	err := s.idb(ctx).NewSelect().
		TableExpr("operation_run AS orun").
		ColumnExpr("orun.id").
		Where(
			"orun.status IN (?)",
			bun.In([]operationrun.OperationRunStatus{
				operationrun.OperationRunStatusPending,
				operationrun.OperationRunStatusRunning,
			}),
		).
		OrderExpr("orun.updated_at ASC").
		Limit(limit).
		Scan(ctx, &ids)
	return ids, err
}

// LockRunnable locks one runnable operation run with SKIP LOCKED.
func (s *PostgresStore) LockRunnable(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	var rows []*dbmodel.OperationRun
	err := s.idb(ctx).NewSelect().
		Model(&rows).
		Where("orun.id = ?", id).
		Where(
			"orun.status IN (?)",
			bun.In([]operationrun.OperationRunStatus{
				operationrun.OperationRunStatusPending,
				operationrun.OperationRunStatusRunning,
			}),
		).
		For("UPDATE SKIP LOCKED").
		Scan(ctx)
	if err != nil {
		return nil, err
	}
	if len(rows) == 0 {
		return nil, nil
	}

	return dao.OperationRunFrom(rows[0]), nil
}

// LockOperationRunTargets locks materialized targets for one run and phase.
func (s *PostgresStore) LockOperationRunTargets(
	ctx context.Context,
	runID uuid.UUID,
	phaseIndex int32,
) ([]*operationrun.OperationRunTarget, error) {
	var rows []dbmodel.OperationRunTarget
	err := s.idb(ctx).NewSelect().
		Model(&rows).
		Where("ort.operation_run_id = ?", runID).
		Where("ort.phase_index = ?", phaseIndex).
		OrderExpr("ort.phase_index ASC, ort.sequence_index ASC").
		For("UPDATE").
		Scan(ctx)
	if err != nil {
		return nil, err
	}

	targets := make([]*operationrun.OperationRunTarget, len(rows))
	for idx := range rows {
		targets[idx] = dao.OperationRunTargetFrom(&rows[idx])
	}
	return targets, nil
}

// GetTargetPhaseAggregate returns aggregate target facts for phase decisions.
func (s *PostgresStore) GetTargetPhaseAggregate(
	ctx context.Context,
	runID uuid.UUID,
	currentPhaseIndex int32,
) (operationrun.TargetPhaseAggregate, error) {
	type aggregateRow struct {
		CompletedTargets    int `bun:"completed_targets"`
		CompletedCompleted  int `bun:"completed_completed"`
		CompletedFailed     int `bun:"completed_failed"`
		CompletedTerminated int `bun:"completed_terminated"`
		CompletedSkipped    int `bun:"completed_skipped"`
	}

	var row aggregateRow
	err := s.idb(ctx).NewSelect().
		TableExpr("operation_run_target AS ort").
		ColumnExpr(
			"COUNT(*) FILTER (WHERE ort.phase_index < ?) AS completed_targets",
			currentPhaseIndex,
		).
		ColumnExpr(
			"COUNT(*) FILTER (WHERE ort.phase_index < ? AND ort.status = ?) AS completed_completed",
			currentPhaseIndex,
			operationrun.OperationRunTargetStatusCompleted,
		).
		ColumnExpr(
			"COUNT(*) FILTER (WHERE ort.phase_index < ? AND ort.status = ?) AS completed_failed",
			currentPhaseIndex,
			operationrun.OperationRunTargetStatusFailed,
		).
		ColumnExpr(
			"COUNT(*) FILTER (WHERE ort.phase_index < ? AND ort.status = ?) AS completed_terminated",
			currentPhaseIndex,
			operationrun.OperationRunTargetStatusTerminated,
		).
		ColumnExpr(
			"COUNT(*) FILTER (WHERE ort.phase_index < ? AND ort.status = ?) AS completed_skipped",
			currentPhaseIndex,
			operationrun.OperationRunTargetStatusSkipped,
		).
		Where("ort.operation_run_id = ?", runID).
		Scan(ctx, &row)
	if err != nil {
		return operationrun.TargetPhaseAggregate{}, err
	}

	return operationrun.TargetPhaseAggregate{
		CompletedPhaseStats: operationrun.PhaseStats{
			PhaseIndex:      max(currentPhaseIndex-1, 0),
			SelectedTargets: row.CompletedTargets,
			StatusCounts: operationrun.TargetStatusCounts{
				Completed:  row.CompletedCompleted,
				Failed:     row.CompletedFailed,
				Terminated: row.CompletedTerminated,
				Skipped:    row.CompletedSkipped,
			},
		},
	}, nil
}

// UpdateRunState persists dispatcher-owned lifecycle fields.
func (s *PostgresStore) UpdateRunState(
	ctx context.Context,
	run *operationrun.OperationRun,
) error {
	if run == nil {
		return fmt.Errorf("operation run is required")
	}

	result, err := s.idb(ctx).NewUpdate().
		TableExpr("operation_run").
		Set("status = ?", run.Status).
		Set("status_reason = ?", run.StatusReason).
		Set("status_message = ?", run.StatusMessage).
		Set("current_phase_index = ?", run.CurrentPhaseIndex).
		Set("started_at = ?", run.StartedAt).
		Set("finished_at = ?", run.FinishedAt).
		Where("id = ?", run.ID).
		Exec(ctx)
	if err != nil {
		return err
	}

	return requireUpdatedRow(result, "operation run", run.ID)
}

// UpdateTargetState persists dispatcher-owned target lifecycle fields.
func (s *PostgresStore) UpdateTargetState(
	ctx context.Context,
	target *operationrun.OperationRunTarget,
) error {
	if target == nil {
		return fmt.Errorf("operation run target is required")
	}

	result, err := s.idb(ctx).NewUpdate().
		TableExpr("operation_run_target").
		Set("task_id = ?", target.TaskID).
		Set("status = ?", target.Status).
		Set("message = ?", target.Message).
		Set("retry_after = ?", target.RetryAfter).
		Set("retry_state = ?", target.RetryState).
		Where("id = ?", target.ID).
		Exec(ctx)
	if err != nil {
		return err
	}

	return requireUpdatedRow(result, "operation run target", target.ID)
}

func requireUpdatedRow(result sql.Result, resource string, id uuid.UUID) error {
	rowsAffected, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("check %s %s update result: %w", resource, id, err)
	}

	if rowsAffected == 0 {
		return fmt.Errorf("%s %s not found", resource, id)
	}

	return nil
}
