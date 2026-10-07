// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// preparedDispatch holds the locked run state and reconciled target summary
// needed to decide the next dispatch transition.
type preparedDispatch struct {
	run            *operationrun.OperationRun
	options        *operationrun.Options
	op             *operationrun.Operation
	conflictPolicy conflictPolicyRuntime
	safetyPolicy   *safetyPolicyRuntime
	phasePolicy    *phasePolicyRuntime
	prepareErr     error
	summary        operationrun.TargetPhaseSummary
	changed        map[uuid.UUID]*operationrun.OperationRunTarget
}

func newPreparedDispatch(run *operationrun.OperationRun) *preparedDispatch {
	return &preparedDispatch{
		run:     run,
		changed: map[uuid.UUID]*operationrun.OperationRunTarget{},
	}
}

func (p *preparedDispatch) hasRuntimeConfiguration() bool {
	return p.options != nil &&
		p.op != nil &&
		p.safetyPolicy != nil &&
		p.phasePolicy != nil &&
		p.conflictPolicy != nil
}

// prepare locks the runnable run, locks its targets, and reconciles child task
// state.
func (d *Dispatcher) prepare(
	ctx context.Context,
	id uuid.UUID,
) (*preparedDispatch, error) {
	run, err := d.deps.Store.LockRunnable(ctx, id)
	if err != nil {
		return nil, fmt.Errorf("lock operation run: %w", err)
	}

	// A nil run means another dispatcher pass or operator action made this run
	// no longer runnable before we acquired the lock. That is normal drift, not
	// a dispatch error.
	if run == nil {
		return nil, nil
	}

	prep := newPreparedDispatch(run)
	prep.options, prep.op, prep.prepareErr = decodeRunConfiguration(run)
	// Handleable preparation errors are kept on prep so the decision/execution
	// phases can fail the run and persist that terminal state in the same flow.
	if prep.prepareErr != nil {
		return prep, nil
	}

	prep.phasePolicy, prep.prepareErr = newPhasePolicy(prep.options)
	if prep.prepareErr != nil {
		return prep, nil
	}

	prep.safetyPolicy, prep.prepareErr = newSafetyPolicy(prep.options)
	if prep.prepareErr != nil {
		return prep, nil
	}

	prep.conflictPolicy, prep.prepareErr = newConflictPolicy(prep.options)
	if prep.prepareErr != nil {
		return prep, nil
	}

	targets, err := d.deps.Store.LockOperationRunTargets(
		ctx,
		run.ID,
		run.CurrentPhaseIndex,
	)
	if err != nil {
		return nil, fmt.Errorf("lock operation run targets: %w", err)
	}

	if err := d.reconcileTargets(ctx, targets, prep.changed); err != nil {
		return nil, err
	}
	aggregate, err := d.deps.Store.GetTargetPhaseAggregate(
		ctx,
		run.ID,
		run.CurrentPhaseIndex,
	)
	if err != nil {
		return nil, fmt.Errorf("summarize operation run targets: %w", err)
	}
	prep.summary = operationrun.NewTargetPhaseSummary(
		run.CurrentPhaseIndex,
		run.TotalPhases,
		aggregate,
		targets,
	)

	return prep, nil
}

// decodeRunConfiguration decodes and validates the persisted user-supplied
// options and operation template before the dispatcher uses them.
func decodeRunConfiguration(
	run *operationrun.OperationRun,
) (*operationrun.Options, *operationrun.Operation, error) {
	options, err := run.DecodedOptions()
	if err != nil {
		return nil, nil, err
	}

	op, err := run.DecodedOperation()
	if err != nil {
		return nil, nil, err
	}

	return options, op, nil
}
