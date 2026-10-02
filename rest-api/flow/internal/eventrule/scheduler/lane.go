// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventexecutor "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
)

type claimExecutionsFunc func(
	context.Context,
	eventrule.ExecutionClaimRequest,
) (eventrule.ExecutionClaimBatch, error)

type laneDefinition struct {
	name          string
	defaultConfig LaneConfig
	claimFunc     func(eventrule.ExecutionStore) claimExecutionsFunc
}

var laneDefinitions = []laneDefinition{
	{
		name: "pending",
		defaultConfig: LaneConfig{
			Workers:   1,
			ScanLimit: 1,
		},
		claimFunc: func(store eventrule.ExecutionStore) claimExecutionsFunc {
			return store.ClaimPendingExecutions
		},
	},
	{
		name: "deferred",
		defaultConfig: LaneConfig{
			Workers:   1,
			ScanLimit: 1,
		},
		claimFunc: func(store eventrule.ExecutionStore) claimExecutionsFunc {
			return store.ClaimRetryExecutions
		},
	},
}

var errLaneCapacityAccounting = errors.New("lane capacity accounting mismatch")

type lane struct {
	name      string
	claimFunc claimExecutionsFunc
	scanLimit int
	jobs      chan eventrule.ClaimedExecution
	slots     chan struct{}
}

func newLane(
	name string,
	config LaneConfig,
	claimFunc claimExecutionsFunc,
) *lane {
	slots := make(chan struct{}, config.Workers)
	for range config.Workers {
		slots <- struct{}{}
	}

	return &lane{
		name:      name,
		claimFunc: claimFunc,
		scanLimit: config.ScanLimit,
		jobs:      make(chan eventrule.ClaimedExecution, config.Workers),
		slots:     slots,
	}
}

func (l *lane) reserveAvailableSlots() int {
	reserved := 0
	for reserved < l.scanLimit {
		select {
		case <-l.slots:
			reserved++
		default:
			return reserved
		}
	}

	return reserved
}

func (l *lane) returnSlots(count int) error {
	for range count {
		if err := l.returnSlot(); err != nil {
			return err
		}
	}

	return nil
}

func (l *lane) returnSlot() error {
	select {
	case l.slots <- struct{}{}:
		return nil
	default:
		return fmt.Errorf(
			"cannot return %s worker slot: %w",
			l.name,
			errLaneCapacityAccounting,
		)
	}
}

func (l *lane) claim(
	ctx context.Context,
	owner string,
	claimDuration time.Duration,
	maxAttempts int,
) (eventrule.ExecutionClaimBatch, error) {
	reserved := l.reserveAvailableSlots()
	if reserved == 0 {
		return eventrule.ExecutionClaimBatch{}, nil
	}

	batch, err := l.claimFunc(
		ctx,
		eventrule.ExecutionClaimRequest{
			Owner:         owner,
			Limit:         reserved,
			ClaimDuration: claimDuration,
			MaxAttempts:   maxAttempts,
		},
	)
	if err != nil {
		slotErr := l.returnSlots(reserved)
		if ctxErr := ctx.Err(); ctxErr != nil && errors.Is(err, ctxErr) {
			return eventrule.ExecutionClaimBatch{}, slotErr
		}

		return eventrule.ExecutionClaimBatch{}, errors.Join(
			fmt.Errorf("claim %s executions: %w", l.name, err),
			slotErr,
		)
	}

	if err := l.returnSlots(reserved - len(batch.Claims)); err != nil {
		return eventrule.ExecutionClaimBatch{}, err
	}

	return batch, nil
}

func (l *lane) startWorkers(
	ctx context.Context,
	workers *sync.WaitGroup,
	runtime *runtime,
) {
	for range cap(l.slots) {
		workers.Go(func() {
			l.runWorker(ctx, runtime)
		})
	}
}

func (l *lane) runWorker(ctx context.Context, runtime *runtime) {
	for {
		select {
		case <-ctx.Done():
			return
		case claim := <-l.jobs:
			if err := l.dispatch(ctx, claim, runtime); err != nil {
				// Outcome persistence is claim-scoped. Report the failure but
				// keep this worker available for unrelated executions.
				l.logExecutionPersistenceError(claim, err)
			}

			if err := l.returnSlot(); err != nil {
				// A failed slot return means lane-capacity accounting is no
				// longer trustworthy, so stop the scheduler through its fatal
				// worker-error path.
				l.reportFatalWorkerError(ctx, err, runtime)
				return
			}

			l.notifyScheduler(runtime)
		}
	}
}

func (l *lane) dispatch(
	ctx context.Context,
	claim eventrule.ClaimedExecution,
	runtime *runtime,
) error {
	execution := claim.Execution
	var result eventrule.ExecutionResult

	executor, err := runtime.executors.Executor(execution.Plan.Type())
	if err != nil {
		result = runtime.policy.resultForExecutionError(execution.Attempts, err)
	} else {
		req := eventexecutor.ExecutionRequest{
			ExecutionID: execution.ID,
			Plan:        execution.Plan,
		}
		if err := executor.Execute(ctx, req); err != nil {
			result = runtime.policy.resultForExecutionError(execution.Attempts, err)
		} else {
			result = eventrule.CompletedExecutionResult()
		}
	}

	return l.persistResult(ctx, claim, result, runtime)
}

func (l *lane) persistResult(
	ctx context.Context,
	claim eventrule.ClaimedExecution,
	result eventrule.ExecutionResult,
	runtime *runtime,
) error {
	persistCtx, cancel := context.WithTimeout(
		context.WithoutCancel(ctx),
		runtime.persistTimeout,
	)
	defer cancel()

	return runtime.store.TransitionClaimedExecution(
		persistCtx,
		claim.Execution.ID,
		claim.Token,
		result,
	)
}

func (l *lane) logExecutionPersistenceError(
	claim eventrule.ClaimedExecution,
	err error,
) {
	event := log.Error()
	if errors.Is(err, eventrule.ErrExecutionClaimLost) {
		event = log.Warn()
	}

	event.
		Err(err).
		Str("lane", l.name).
		Str("action_name", claim.Execution.ActionName).
		Stringer("execution_id", claim.Execution.ID).
		Msg("failed to persist event-rule execution outcome")
}

func (l *lane) reportFatalWorkerError(
	ctx context.Context,
	err error,
	runtime *runtime,
) {
	select {
	case runtime.fatalWorkerErrors <- err:
	case <-ctx.Done():
	}
}

func (l *lane) notifyScheduler(runtime *runtime) {
	select {
	case runtime.wakeCh <- struct{}{}:
	default:
	}
}
