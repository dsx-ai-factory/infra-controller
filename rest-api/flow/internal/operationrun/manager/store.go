// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"

	"github.com/google/uuid"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// Store is the operation-run persistence surface used by Manager.
type Store interface {
	// Create inserts one operation_run row in the initial pending state.
	Create(ctx context.Context, run *operationrun.OperationRun) (uuid.UUID, error)

	// Get returns the operation run with the given ID, or an error if not found.
	Get(ctx context.Context, id uuid.UUID) (*operationrun.OperationRun, error)

	// LockOperationRun locks one operation run for manual lifecycle changes.
	LockOperationRun(
		ctx context.Context,
		id uuid.UUID,
	) (*operationrun.OperationRun, error)

	// List returns operation runs matching opts, along with the total count
	// before pagination is applied. Selector and operation template are not
	// populated because list responses use the lightweight summary shape.
	List(
		ctx context.Context,
		opts operationrun.ListOptions,
	) ([]*operationrun.OperationRun, int32, error)

	// CreateTargets inserts materialized rack execution targets for a run. The
	// manager calls this after planning to persist the frozen execution plan.
	CreateTargets(
		ctx context.Context,
		runID uuid.UUID,
		targets []*operationrun.OperationRunTarget,
	) error

	// ListTargets returns targets for one operation run, along with the total
	// count before pagination is applied.
	ListTargets(
		ctx context.Context,
		runID uuid.UUID,
		opts operationrun.TargetListOptions,
	) ([]*operationrun.OperationRunTarget, int32, error)

	// LockOperationRunTargets locks materialized targets for one run and phase.
	LockOperationRunTargets(
		ctx context.Context,
		runID uuid.UUID,
		phaseIndex int32,
	) ([]*operationrun.OperationRunTarget, error)

	// GetTargetPhaseAggregate returns aggregate target facts for phase decisions.
	GetTargetPhaseAggregate(
		ctx context.Context,
		runID uuid.UUID,
		currentPhaseIndex int32,
	) (operationrun.TargetPhaseAggregate, error)

	// UpdateRunState persists lifecycle fields owned by dispatch/manual
	// controls.
	UpdateRunState(ctx context.Context, run *operationrun.OperationRun) error

	// UpdateTargetState persists target lifecycle fields owned by
	// dispatch/manual controls.
	UpdateTargetState(
		ctx context.Context,
		target *operationrun.OperationRunTarget,
	) error

	// RunInTransaction executes fn within a database transaction. The
	// transaction is propagated through ctx so nested Store calls participate in
	// it automatically. fn must use the ctx it receives.
	RunInTransaction(ctx context.Context, fn func(ctx context.Context) error) error
}
