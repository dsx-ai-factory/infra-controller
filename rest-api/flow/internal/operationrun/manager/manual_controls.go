// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

// operationRunControl mutates a locked run inside applyOperationRunControl's
// transaction.
type operationRunControl func(
	ctx context.Context,
	run *operationrun.OperationRun,
) (*operationrun.OperationRun, error)

const (
	// cancelTasksTimeout bounds best-effort child task cancellation after the
	// run cancellation is durably committed.
	cancelTasksTimeout = 5 * time.Second
	// resumeExpectedPhaseIndex marks the shared continue helper as a resume
	// request rather than a phase advance.
	resumeExpectedPhaseIndex int32 = -1
	// advanceWithoutExpectedPhaseIndex marks a phase advance with no optimistic
	// expected-phase check.
	advanceWithoutExpectedPhaseIndex int32 = 0
)

// Pause applies an operator pause to a pending or running run. Already-paused
// runs are returned unchanged so the original pause reason remains visible.
func (m *ManagerImpl) Pause(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	return m.applyOperationRunControl(
		ctx,
		id,
		func(
			txCtx context.Context,
			run *operationrun.OperationRun,
		) (*operationrun.OperationRun, error) {
			if !run.CanPause() {
				return nil, invalidStateError(
					"%s is in terminal state %s",
					run.ID,
					run.Status,
				)
			}
			// Pause is idempotent; keep the existing reason so operator
			// pause does not hide a safety, conflict, or phase-gate pause.
			if run.Status == operationrun.OperationRunStatusPaused {
				return run, nil
			}

			run.Pause(
				operationrun.OperationRunStatusReasonOperatorPaused,
				"operation run paused by operator",
			)

			if err := m.store.UpdateRunState(txCtx, run); err != nil {
				return nil, err
			}

			return run, nil
		},
	)
}

// Resume clears a non-phase-gate pause and lets the dispatcher continue the
// current phase. Manual phase gates must use AdvancePhase so callers do not
// accidentally cross a rollout boundary.
func (m *ManagerImpl) Resume(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	return m.applyOperationRunControl(
		ctx,
		id,
		m.continueOperationRun(resumeExpectedPhaseIndex),
	)
}

// AdvancePhase opens the next phase for a run paused at a manual phase gate.
func (m *ManagerImpl) AdvancePhase(
	ctx context.Context,
	id uuid.UUID,
	expectedPhaseIndex *int32,
) (*operationrun.OperationRun, error) {
	expectedPhase := advanceWithoutExpectedPhaseIndex
	if expectedPhaseIndex != nil &&
		*expectedPhaseIndex > advanceWithoutExpectedPhaseIndex {
		expectedPhase = *expectedPhaseIndex
	}

	return m.applyOperationRunControl(
		ctx,
		id,
		m.continueOperationRun(expectedPhase),
	)
}

// Cancel marks a non-terminal run cancelled and best-effort cancels submitted
// current-phase child tasks. Terminal runs are returned unchanged.
func (m *ManagerImpl) Cancel(
	ctx context.Context,
	id uuid.UUID,
	reason string,
	canceller TaskCanceller,
) (*operationrun.OperationRun, error) {
	if canceller == nil {
		return nil, fmt.Errorf("task canceller is required")
	}

	reason = strings.TrimSpace(reason)
	message := "operation run cancelled"
	if reason != "" {
		message += ": " + reason
	}

	cancelledRunID := uuid.Nil
	result, err := m.applyOperationRunControl(
		ctx,
		id,
		func(
			txCtx context.Context,
			run *operationrun.OperationRun,
		) (*operationrun.OperationRun, error) {
			if run.Status.IsTerminal() {
				return run, nil
			}

			run.Cancel(time.Now().UTC(), message)
			if err := m.store.UpdateRunState(txCtx, run); err != nil {
				return nil, err
			}

			cancelledRunID = run.ID
			return run, nil
		},
	)

	if err == nil && cancelledRunID != uuid.Nil {
		// The cancelled run state is already durable. Child task cancellation is
		// best-effort cleanup and must not change the RPC result.
		cleanupCtx, cleanupCancel := context.WithTimeout(
			context.WithoutCancel(ctx),
			cancelTasksTimeout,
		)
		defer cleanupCancel()

		m.cancelTasks(cleanupCtx, cancelledRunID, canceller)
	}

	return result, err
}

func (m *ManagerImpl) cancelTasks(
	ctx context.Context,
	runID uuid.UUID,
	canceller TaskCanceller,
) {
	targets, _, err := m.store.ListTargets(
		ctx,
		runID,
		operationrun.TargetListOptions{
			PhaseScope: operationrun.TargetPhaseScopeCurrentPhase,
		},
	)
	if err != nil {
		log.Warn().
			Err(err).
			Str("operation_run_id", runID.String()).
			Msg("operation run cancel: failed to find submitted child tasks")
		return
	}

	for _, target := range targets {
		if target == nil || target.Status.IsTerminal() || target.TaskID == nil {
			continue
		}

		if err := canceller.CancelTask(ctx, *target.TaskID); err != nil {
			log.Warn().
				Err(err).
				Str("operation_run_id", target.OperationRunID.String()).
				Str("operation_run_target_id", target.ID.String()).
				Str("task_id", target.TaskID.String()).
				Msg("operation run cancel: failed to cancel child task")
		}
	}
}

func (m *ManagerImpl) applyOperationRunControl(
	ctx context.Context,
	id uuid.UUID,
	control operationRunControl,
) (*operationrun.OperationRun, error) {
	if err := m.requireDependencies(); err != nil {
		return nil, err
	}

	if id == uuid.Nil {
		return nil, fmt.Errorf("operation run ID is required")
	}

	var result *operationrun.OperationRun
	err := m.store.RunInTransaction(
		ctx,
		func(txCtx context.Context) error {
			run, err := m.lockOperationRun(txCtx, id)
			if err != nil {
				return err
			}

			if run == nil {
				return ErrOperationRunRequired
			}

			result, err = control(txCtx, run)
			return err
		},
	)

	if err != nil {
		return nil, err
	}

	return result, nil
}

func (m *ManagerImpl) lockOperationRun(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	run, err := m.store.LockOperationRun(ctx, id)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, fmt.Errorf("%w: %s", ErrOperationRunNotFound, id)
		}
		return nil, err
	}

	return run, nil
}

func (m *ManagerImpl) continueOperationRun(
	expectedPhaseIndex int32,
) operationRunControl {
	return func(
		txCtx context.Context,
		run *operationrun.OperationRun,
	) (*operationrun.OperationRun, error) {
		if err := ensureCanContinueRun(run, expectedPhaseIndex); err != nil {
			return nil, err
		}

		summary, err := m.targetPhaseSummaryForContinue(
			txCtx,
			run,
			expectedPhaseIndex,
		)
		if err != nil {
			return nil, err
		}

		now := time.Now().UTC()

		if status, ok := summary.TerminalRunStatus(); ok {
			// If target reconciliation already reached a terminal outcome,
			// record that final run state instead of restarting dispatcher
			// work.
			switch status {
			case operationrun.OperationRunStatusFailed:
				run.Fail(now, status.Message())
			case operationrun.OperationRunStatusCompletedWithFailures:
				run.CompleteWithFailures(now, status.Message())
			case operationrun.OperationRunStatusCompleted:
				run.Complete(now, status.Message())
			default:
				return nil, fmt.Errorf(
					"unexpected terminal operation run status %s",
					status,
				)
			}
		} else {
			// Otherwise mark the run as started so the dispatcher can continue
			// the current phase or begin the newly advanced phase.
			switch {
			case expectedPhaseIndex == resumeExpectedPhaseIndex:
				run.Start(now, "operation run resumed")
			case expectedPhaseIndex >= advanceWithoutExpectedPhaseIndex:
				run.CurrentPhaseIndex++
				run.Start(
					now,
					fmt.Sprintf(
						"advanced to phase %d",
						run.CurrentPhaseIndex,
					),
				)
			default:
				return nil, fmt.Errorf(
					"invalid internal expected phase index %d",
					expectedPhaseIndex,
				)
			}
		}

		if err := m.store.UpdateRunState(txCtx, run); err != nil {
			return nil, err
		}

		return run, nil
	}
}

func ensureCanContinueRun(
	run *operationrun.OperationRun,
	expectedPhaseIndex int32,
) error {
	switch {
	case expectedPhaseIndex == resumeExpectedPhaseIndex:
		if run.CanResume() {
			return nil
		}

		if run.Status != operationrun.OperationRunStatusPaused {
			return invalidStateError(
				"%s is not paused",
				run.ID,
			)
		}

		if run.StatusReason == operationrun.OperationRunStatusReasonPhaseGate {
			return invalidStateError(
				"%s is paused at a phase gate; use AdvanceOperationRunPhase",
				run.ID,
			)
		}

		return invalidStateError(
			"%s cannot be resumed from status %s with reason %s",
			run.ID,
			run.Status,
			run.StatusReason,
		)
	case expectedPhaseIndex >= advanceWithoutExpectedPhaseIndex:
		if run.CanAdvancePhase() {
			return nil
		}

		return invalidStateError(
			"%s is not paused at a phase gate",
			run.ID,
		)
	default:
		return invalidStateError(
			"invalid internal expected phase index %d",
			expectedPhaseIndex,
		)
	}
}

func invalidStateError(format string, args ...any) error {
	return fmt.Errorf(
		"%w: "+format,
		append([]any{ErrOperationRunInvalidState}, args...)...,
	)
}

func ensurePhaseCanAdvance(
	summary operationrun.TargetPhaseSummary,
	expectedPhaseIndex int32,
) error {
	if !summary.CurrentPhaseTerminal() {
		return invalidStateError(
			"phase %d is not complete",
			summary.CurrentPhaseStats.PhaseIndex,
		)
	}

	if !summary.HasNextPhase() {
		return invalidStateError(
			"phase %d is the final phase",
			summary.CurrentPhaseStats.PhaseIndex,
		)
	}

	nextPhaseIndex := summary.CurrentPhaseStats.PhaseIndex + 1
	if expectedPhaseIndex > 0 && expectedPhaseIndex != nextPhaseIndex {
		return invalidStateError(
			"expected phase %d, next phase is %d",
			expectedPhaseIndex,
			nextPhaseIndex,
		)
	}
	return nil
}

func (m *ManagerImpl) targetPhaseSummaryForContinue(
	ctx context.Context,
	run *operationrun.OperationRun,
	expectedPhaseIndex int32,
) (operationrun.TargetPhaseSummary, error) {
	emptySummary := operationrun.TargetPhaseSummary{}

	targets, err := m.store.LockOperationRunTargets(
		ctx,
		run.ID,
		run.CurrentPhaseIndex,
	)
	if err != nil {
		return emptySummary, err
	}

	aggregate, err := m.store.GetTargetPhaseAggregate(
		ctx,
		run.ID,
		run.CurrentPhaseIndex,
	)
	if err != nil {
		return emptySummary, err
	}

	summary := operationrun.NewTargetPhaseSummary(
		run.CurrentPhaseIndex,
		run.TotalPhases,
		aggregate,
		targets,
	)
	if _, ok := summary.TerminalRunStatus(); ok {
		return summary, nil
	}

	if expectedPhaseIndex >= advanceWithoutExpectedPhaseIndex {
		if err := ensurePhaseCanAdvance(summary, expectedPhaseIndex); err != nil {
			return emptySummary, err
		}
	}

	// Manual controls only reopen the run. The dispatcher owns safety-gate
	// evaluation before it starts additional target work.
	return summary, nil
}
