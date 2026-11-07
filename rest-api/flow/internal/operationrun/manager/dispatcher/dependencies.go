// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// Store is the operation-run persistence surface used by Dispatcher.
type Store interface {
	RunInTransaction(ctx context.Context, fn func(ctx context.Context) error) error
	FetchRunnableIDs(ctx context.Context, limit int) ([]uuid.UUID, error)
	LockRunnable(ctx context.Context, id uuid.UUID) (*operationrun.OperationRun, error)
	LockOperationRunTargets(ctx context.Context, runID uuid.UUID, phaseIndex int32) ([]*operationrun.OperationRunTarget, error)
	GetTargetPhaseAggregate(ctx context.Context, runID uuid.UUID, currentPhaseIndex int32) (operationrun.TargetPhaseAggregate, error)
	UpdateRunState(ctx context.Context, run *operationrun.OperationRun) error
	UpdateTargetState(ctx context.Context, target *operationrun.OperationRunTarget) error
}

// TaskManager is the child-task submission surface used by the dispatcher.
type TaskManager interface {
	SubmitTask(ctx context.Context, req *operation.Request) ([]uuid.UUID, error)
}

// TaskStore is the child-task read surface used for target reconciliation.
type TaskStore interface {
	GetTask(ctx context.Context, id uuid.UUID) (*taskdef.Task, error)
}

// Dependencies holds the required collaborators used by Dispatcher.
type Dependencies struct {
	Store       Store
	TaskManager TaskManager
	TaskStore   TaskStore
}

func (d Dependencies) validate() error {
	if d.Store == nil {
		return fmt.Errorf("operation run store is required")
	}
	if d.TaskManager == nil {
		return fmt.Errorf("task manager is required")
	}
	if d.TaskStore == nil {
		return fmt.Errorf("task store is required")
	}
	return nil
}
