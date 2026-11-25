// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
)

const (
	reconciliationInterval    = time.Minute
	reconciliationBatchSize   = 100
	reconciliationCallTimeout = 5 * time.Second
)

// Reconcile persisted tasks immediately after startup and periodically thereafter.
// One sequential worker bounds Temporal load; each RPC gets its own deadline so
// an unavailable execution cannot starve later tasks in the sweep.
func (m *ManagerImpl) reconcileLoop(ctx context.Context) {
	defer m.reconcileWG.Done()
	ticker := time.NewTicker(reconciliationInterval)
	defer ticker.Stop()
	for {
		m.reconcileExecutions(ctx)
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func (m *ManagerImpl) reconcileExecutions(ctx context.Context) {
	afterID := uuid.Nil
	for ctx.Err() == nil {
		queryCtx, cancel := context.WithTimeout(ctx, reconciliationCallTimeout)
		tasks, err := m.taskStore.ListTasksForReconciliation(queryCtx, m.executor.Type(), afterID, reconciliationBatchSize)
		cancel()
		if err != nil {
			log.Warn().Err(err).Msg("failed to list tasks for execution reconciliation")
			return
		}
		for _, t := range tasks {
			if ctx.Err() != nil {
				return
			}
			afterID = t.ID
			callCtx, cancel := context.WithTimeout(ctx, reconciliationCallTimeout)
			status, err := m.executor.CheckStatus(callCtx, t.ExecutionID)
			cancel()
			if err != nil {
				log.Warn().Err(err).Str("task_id", t.ID.String()).Msg("failed to reconcile task execution")
				continue
			}
			if status == nil || !status.Status.IsFinished() || status.FinishedAt == nil {
				continue
			}
			updateCtx, cancel := context.WithTimeout(ctx, reconciliationCallTimeout)
			updated, err := m.taskStore.FinishTaskExecution(updateCtx, t, status)
			cancel()
			if err != nil {
				log.Warn().Err(err).Str("task_id", t.ID.String()).Msg("failed to persist closed task execution")
				continue
			}
			if updated {
				m.promoter.Notify(t.RackID)
			}
		}
		if len(tasks) < reconciliationBatchSize {
			return
		}
	}
}
