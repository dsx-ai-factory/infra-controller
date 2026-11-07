// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// dispatchRun advances one runnable operation run, then submits any targets
// claimed by the transaction. If post-commit submission fails to start active
// work, it makes one follow-up attempt to fill the freed quota slots.
func (d *Dispatcher) dispatchRun(
	ctx context.Context,
	id uuid.UUID,
) error {
	now := d.now()
	retryDispatch, err := d.dispatchRunAttempt(ctx, id, now)
	if err != nil || !retryDispatch {
		return err
	}

	_, err = d.dispatchRunAttempt(ctx, id, now)
	return err
}

func (d *Dispatcher) dispatchRunAttempt(
	ctx context.Context,
	id uuid.UUID,
	now time.Time,
) (bool, error) {
	var claimed claimedTargets

	err := d.deps.Store.RunInTransaction(
		ctx,
		func(txCtx context.Context) error {
			var err error
			claimed, err = d.dispatchRunInTransaction(txCtx, id, now)
			return err
		},
	)
	if err != nil {
		return false, err
	}

	retryDispatch := false
	for i, target := range claimed.targets {
		startedActiveTask, err := d.submit(
			ctx,
			claimed.conflictPolicy,
			target,
			claimed.requests[i],
			now,
		)
		if err != nil {
			// Do not retry immediately when the post-submit state write fails:
			// task submission may already have succeeded, so the claim lease is
			// the recovery path until submissions become idempotent.
			log.Error().
				Err(err).
				Str("operation_run_target_id", target.ID.String()).
				Msg("operation run dispatcher: failed to write submission result")
			continue
		}
		if !startedActiveTask {
			retryDispatch = true
		}
	}

	return retryDispatch, nil
}

// dispatchRunInTransaction owns the DB-locked portion of one dispatch attempt:
// prepare the run, decide its next transition, and claim targets for
// submission. It returns claimed submissions to execute after the transaction
// commits so child task creation does not hold operation-run locks.
func (d *Dispatcher) dispatchRunInTransaction(
	ctx context.Context,
	id uuid.UUID,
	now time.Time,
) (claimedTargets, error) {
	prep, err := d.prepare(ctx, id)
	if err != nil {
		return claimedTargets{}, err
	}
	if prep == nil {
		return claimedTargets{}, nil
	}

	decision, err := d.decide(prep, now)
	if err != nil {
		return claimedTargets{}, err
	}

	claimed, err := d.execute(prep, decision, now)
	if err != nil {
		return claimedTargets{}, err
	}

	if err := d.persistDispatchState(ctx, prep.run, prep.changed); err != nil {
		return claimedTargets{}, err
	}

	return claimed, nil
}

// persistDispatchState writes target changes before the parent run state so the
// run summary never advances ahead of the target rows it depends on.
func (d *Dispatcher) persistDispatchState(
	ctx context.Context,
	run *operationrun.OperationRun,
	changed map[uuid.UUID]*operationrun.OperationRunTarget,
) error {
	for _, target := range changed {
		if err := d.deps.Store.UpdateTargetState(ctx, target); err != nil {
			return fmt.Errorf("update target %s: %w", target.ID, err)
		}
	}

	if err := d.deps.Store.UpdateRunState(ctx, run); err != nil {
		return fmt.Errorf("update operation run %s: %w", run.ID, err)
	}
	return nil
}
