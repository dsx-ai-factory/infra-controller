// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	operationrunplanner "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun/manager/planner"
)

var (
	// ErrOperationRunRequired reports that Create was called without a run.
	ErrOperationRunRequired = errors.New("operation run is required")
	// ErrOperationRunNotFound reports that an operation run ID does not match
	// a persisted run.
	ErrOperationRunNotFound = errors.New("operation run not found")
	// ErrNoPlannedTargets reports that planning produced no executable targets.
	ErrNoPlannedTargets = errors.New("operation run has no planned targets")
	// ErrOperationRunInvalidPlan reports that a valid request shape cannot be
	// materialized for its selected target set.
	ErrOperationRunInvalidPlan = errors.New("operation run plan is invalid")
	// ErrOperationRunInvalidState reports that a manual lifecycle operation
	// cannot be applied to the run's current state.
	ErrOperationRunInvalidState = errors.New("operation run is not in a valid state for the requested operation")
	// ErrOperationRunSafetyGateTripped reports that resuming would immediately
	// violate the configured safety policy.
	ErrOperationRunSafetyGateTripped = errors.New("operation run safety gate is tripped")
)

// Create persists an operation run and its frozen planned targets.
func (m *ManagerImpl) Create(
	ctx context.Context,
	run *operationrun.OperationRun,
) (uuid.UUID, error) {
	if err := m.requireDependencies(); err != nil {
		return uuid.Nil, err
	}

	if run == nil {
		return uuid.Nil, ErrOperationRunRequired
	}

	targets, err := m.planner.Plan(ctx, run)
	if err != nil {
		if errors.Is(err, operationrunplanner.ErrPhaseHasNoTargets) {
			return uuid.Nil, fmt.Errorf("%w: %v", ErrOperationRunInvalidPlan, err)
		}
		return uuid.Nil, fmt.Errorf("plan operation run targets: %w", err)
	}
	if len(targets) == 0 {
		return uuid.Nil, ErrNoPlannedTargets
	}

	run.TotalPhases = 0
	for _, target := range targets {
		if phaseCount := target.PhaseIndex + 1; phaseCount > run.TotalPhases {
			run.TotalPhases = phaseCount
		}
	}

	var id uuid.UUID
	transactionFn := func(txCtx context.Context) error {
		runID, err := m.store.Create(txCtx, run)
		if err != nil {
			return err
		}

		if err := m.store.CreateTargets(txCtx, runID, targets); err != nil {
			return err
		}

		id = runID
		return nil
	}

	if err := m.store.RunInTransaction(ctx, transactionFn); err != nil {
		return uuid.Nil, fmt.Errorf("create operation run: %w", err)
	}

	return id, nil
}
