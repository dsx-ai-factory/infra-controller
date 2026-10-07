// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"database/sql"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun/manager/planner"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

var _ Store = (*mockStore)(nil)
var _ planner.TargetLookup = (*mockTargetLookup)(nil)

func TestCreatePersistsRunAndPlannedTargets(t *testing.T) {
	runID := uuid.New()
	store := &mockStore{runID: runID}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{
		defaultScope: testExecutionTargets(3),
	}, planner.Config{}))
	run := testOperationRun(t)
	run.TotalPhases = 99

	got, err := manager.Create(context.Background(), run)
	require.NoError(t, err)
	require.Equal(t, runID, got)

	require.Equal(t, 1, store.txCalls)
	require.Equal(t, 1, store.createCalls)
	require.Equal(t, int32(2), store.createdRun.TotalPhases)
	require.Len(t, store.createdTargets, 3)
	require.Equal(t, []int32{0, 0, 1}, targetPhaseIndexes(store.createdTargets))
	require.Equal(t, []int32{0, 1, 2}, targetSequenceIndexes(store.createdTargets))
	for _, target := range store.createdTargets {
		require.Equal(t, runID, target.OperationRunID)
		require.Equal(t, operationrun.OperationRunTargetStatusPending, target.Status)
	}
}

func TestCreateRejectsEmptyPlannedTargetsBeforeStoreWrite(t *testing.T) {
	store := &mockStore{runID: uuid.New()}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	_, err := manager.Create(context.Background(), testOperationRun(t))
	require.ErrorIs(t, err, ErrNoPlannedTargets)
	require.ErrorContains(t, err, "operation run has no planned targets")
	require.Zero(t, store.txCalls)
	require.Zero(t, store.createCalls)
	require.Zero(t, store.createTargetsCalls)
}

func TestCreateRejectsPhaseThatRoundsToZeroBeforeStoreWrite(t *testing.T) {
	store := &mockStore{runID: uuid.New()}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{
		defaultScope: testExecutionTargets(2),
	}, planner.Config{}))
	run := testOperationRun(t)

	var options operationrun.Options
	require.NoError(t, operationrun.UnmarshalConfig(run.Options, &options))
	options.PhasePolicy.Plan = &operationrun.PercentagePhases{
		Phases: []operationrun.PercentagePhase{
			{Percentage: 10},
			{Percentage: 30},
			{Percentage: 60},
		},
	}
	var err error
	run.Options, err = operationrun.MarshalConfig(options)
	require.NoError(t, err)

	_, err = manager.Create(context.Background(), run)

	require.ErrorIs(t, err, ErrOperationRunInvalidPlan)
	require.ErrorContains(
		t,
		err,
		"percentage phase 2 resolves to zero targets for 2 selected targets",
	)
	require.Zero(t, store.txCalls)
	require.Zero(t, store.createCalls)
	require.Zero(t, store.createTargetsCalls)
}

func TestCreateRejectsNilRunBeforeStoreWrite(t *testing.T) {
	store := &mockStore{runID: uuid.New()}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	_, err := manager.Create(context.Background(), nil)

	require.ErrorIs(t, err, ErrOperationRunRequired)
	require.ErrorContains(t, err, "operation run is required")
	require.Zero(t, store.txCalls)
	require.Zero(t, store.createCalls)
	require.Zero(t, store.createTargetsCalls)
}

func TestNewRejectsMissingDependencies(t *testing.T) {
	store := &mockStore{runID: uuid.New()}
	plan := planner.New(&mockTargetLookup{}, planner.Config{})

	_, err := New(nil, plan)
	require.ErrorContains(t, err, "operation run store is required")

	_, err = New(store, nil)
	require.ErrorContains(t, err, "operation run planner is required")
}

func TestGetMapsStoreNoRowsToNotFound(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		getErr: fmt.Errorf("operation run %s not found: %w", runID, sql.ErrNoRows),
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Get(context.Background(), runID)

	require.Nil(t, got)
	require.ErrorIs(t, err, ErrOperationRunNotFound)
	require.ErrorContains(t, err, runID.String())
}

func TestListTargetsMapsStoreNoRowsToNotFound(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		listTargetsErr: fmt.Errorf("operation run %s not found: %w", runID, sql.ErrNoRows),
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	targets, total, err := manager.ListTargets(
		context.Background(),
		runID,
		operationrun.TargetListOptions{},
	)

	require.Nil(t, targets)
	require.Zero(t, total)
	require.ErrorIs(t, err, ErrOperationRunNotFound)
	require.ErrorContains(t, err, runID.String())
}

func TestPauseMarksRunOperatorPaused(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusRunning,
			operationrun.OperationRunStatusReasonNone,
		),
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Pause(context.Background(), runID)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, 1, store.txCalls)
	require.Equal(t, 1, store.updateRunCalls)
	require.Equal(t, operationrun.OperationRunStatusPaused, store.updatedRun.Status)
	require.Equal(
		t,
		operationrun.OperationRunStatusReasonOperatorPaused,
		store.updatedRun.StatusReason,
	)
}

func TestPauseLeavesAlreadyPausedRunUnchanged(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonSafetyGate,
		),
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Pause(context.Background(), runID)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, 1, store.txCalls)
	require.Zero(t, store.updateRunCalls)
	require.Equal(
		t,
		operationrun.OperationRunStatusReasonSafetyGate,
		got.StatusReason,
	)
}

func TestPauseRejectsNilLockedRun(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{lockRunNil: true}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Pause(context.Background(), runID)

	require.Nil(t, got)
	require.ErrorIs(t, err, ErrOperationRunRequired)
	require.Equal(t, 1, store.txCalls)
}

func TestResumeRejectsPhaseGatePause(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonPhaseGate,
		),
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Resume(context.Background(), runID)

	require.Nil(t, got)
	require.ErrorIs(t, err, ErrOperationRunInvalidState)
	require.ErrorContains(t, err, "AdvanceOperationRunPhase")
	require.Zero(t, store.updateRunCalls)
}

func TestResumeLeavesSafetyGatesToDispatcher(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonOperatorPaused,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusFailed),
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusPending),
		},
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Resume(context.Background(), runID)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, []string{
		"lock_run",
		"lock_targets",
		"update_run",
	}, store.events)
	require.Equal(t, 1, store.updateRunCalls)
	require.Equal(t, operationrun.OperationRunStatusRunning, store.updatedRun.Status)
	require.Equal(
		t,
		operationrun.OperationRunStatusReasonNone,
		store.updatedRun.StatusReason,
	)
	require.Equal(t, "operation run resumed", store.updatedRun.StatusMessage)
}

func TestAdvancePhaseStartsNextPhase(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonPhaseGate,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusCompleted),
			testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusPending),
		},
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))
	expectedPhase := int32(1)

	got, err := manager.AdvancePhase(context.Background(), runID, &expectedPhase)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, 1, store.updateRunCalls)
	require.Equal(t, operationrun.OperationRunStatusRunning, store.updatedRun.Status)
	require.Equal(t, operationrun.OperationRunStatusReasonNone, store.updatedRun.StatusReason)
	require.Equal(t, "advanced to phase 1", store.updatedRun.StatusMessage)
}

func TestAdvancePhaseLeavesSafetyGatesToDispatcher(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	run := testLockedRun(
		t,
		runID,
		operationrun.OperationRunStatusPaused,
		operationrun.OperationRunStatusReasonPhaseGate,
	)
	setOperationRunSafetyPolicy(t, run, operationrun.SafetyPolicy{
		Gates: []operationrun.SafetyGate{
			&operationrun.FailureCountGate{
				Scope:                 operationrun.SafetyGateScopeCumulativeRun,
				FailureThresholdCount: 1,
			},
		},
	})
	store := &mockStore{
		lockRun: run,
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusFailed),
			testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusPending),
		},
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))
	expectedPhase := int32(1)

	got, err := manager.AdvancePhase(context.Background(), runID, &expectedPhase)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, []string{
		"lock_run",
		"lock_targets",
		"update_run",
	}, store.events)
	require.Equal(t, 1, store.updateRunCalls)
	require.Equal(t, operationrun.OperationRunStatusRunning, store.updatedRun.Status)
	require.Equal(
		t,
		operationrun.OperationRunStatusReasonNone,
		store.updatedRun.StatusReason,
	)
	require.Equal(t, "advanced to phase 1", store.updatedRun.StatusMessage)
}

func TestAdvancePhaseChecksExpectedPhase(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonPhaseGate,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusCompleted),
			testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusPending),
		},
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))
	expectedPhase := int32(2)

	got, err := manager.AdvancePhase(context.Background(), runID, &expectedPhase)

	require.Nil(t, got)
	require.ErrorIs(t, err, ErrOperationRunInvalidState)
	require.ErrorContains(t, err, "expected phase 2, next phase is 1")
	require.Zero(t, store.updateRunCalls)
}

func TestAdvancePhaseRejectsExpectedIndexAcrossLegacyEmptyPhases(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonPhaseGate,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusCompleted),
			testOperationRunTarget(runID, 3, operationrun.OperationRunTargetStatusPending),
		},
	}
	store.lockRun.TotalPhases = 4
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))
	expectedPhase := int32(3)

	got, err := manager.AdvancePhase(context.Background(), runID, &expectedPhase)

	require.Nil(t, got)
	require.ErrorIs(t, err, ErrOperationRunInvalidState)
	require.ErrorContains(t, err, "expected phase 3, next phase is 1")
}

func TestAdvancePhaseCompletesAllTerminalRun(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonPhaseGate,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusCompleted),
			testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusCompleted),
		},
	}
	store.lockRun.CurrentPhaseIndex = 1
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))
	expectedPhase := int32(2)

	got, err := manager.AdvancePhase(context.Background(), runID, &expectedPhase)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, 1, store.updateRunCalls)
	require.Equal(t, operationrun.OperationRunStatusCompleted, store.updatedRun.Status)
	require.Equal(t, operationrun.OperationRunStatusReasonNone, store.updatedRun.StatusReason)
	require.Equal(t, "operation run completed", store.updatedRun.StatusMessage)
	require.NotNil(t, store.updatedRun.FinishedAt)
}

func TestAdvancePhaseRejectsInitialPhase(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusPaused,
			operationrun.OperationRunStatusReasonPhaseGate,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusPending),
		},
	}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.AdvancePhase(context.Background(), runID, nil)

	require.Nil(t, got)
	require.ErrorIs(t, err, ErrOperationRunInvalidState)
	require.ErrorContains(t, err, "phase 0 is not complete")
	require.Zero(t, store.updateRunCalls)
}

func TestCancelLeavesTargetsUnchangedAndAttemptsSubmittedCurrentPhaseTasks(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	taskID := uuid.MustParse("22222222-2222-2222-2222-222222222222")
	pendingTarget := testOperationRunTarget(
		runID,
		0,
		operationrun.OperationRunTargetStatusPending,
	)
	submittedTarget := testOperationRunTargetWithTask(
		runID,
		0,
		operationrun.OperationRunTargetStatusSubmitted,
		taskID,
	)
	submittedTarget.Message = "submitted"
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusRunning,
			operationrun.OperationRunStatusReasonNone,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			pendingTarget,
			submittedTarget,
			testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusPending),
		},
	}
	canceller := &mockTaskCanceller{}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Cancel(context.Background(), runID, "operator requested", canceller)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, []string{
		"lock_run",
		"update_run",
		"list_targets",
	}, store.events)
	require.Equal(t, []uuid.UUID{taskID}, canceller.cancelledTaskIDs)
	require.Equal(t, operationrun.OperationRunStatusCancelled, store.updatedRun.Status)
	require.Equal(t, "operation run cancelled: operator requested", store.updatedRun.StatusMessage)
	require.Empty(t, store.updatedTargets)
	require.Equal(t, operationrun.OperationRunTargetStatusPending, pendingTarget.Status)
	require.Equal(t, operationrun.OperationRunTargetStatusSubmitted, submittedTarget.Status)
	require.Equal(t, "submitted", submittedTarget.Message)
}

func TestCancelChildCancelFailureDoesNotPersistTargetFailureMessage(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	taskID := uuid.MustParse("22222222-2222-2222-2222-222222222222")
	target := testOperationRunTargetWithTask(
		runID,
		0,
		operationrun.OperationRunTargetStatusSubmitted,
		taskID,
	)
	target.Message = "submitted"
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusRunning,
			operationrun.OperationRunStatusReasonNone,
		),
		lockedTargets: []*operationrun.OperationRunTarget{target},
	}
	canceller := &mockTaskCanceller{cancelErr: fmt.Errorf("cancel unavailable")}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Cancel(context.Background(), runID, "operator requested", canceller)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, []uuid.UUID{taskID}, canceller.cancelledTaskIDs)
	require.Equal(t, operationrun.OperationRunStatusCancelled, store.updatedRun.Status)
	require.Empty(t, store.updatedTargets)
	require.Equal(t, operationrun.OperationRunTargetStatusSubmitted, target.Status)
	require.Equal(t, "submitted", target.Message)
}

func TestCancelReturnsCancelledRunWhenSubmittedTargetDiscoveryFails(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusRunning,
			operationrun.OperationRunStatusReasonNone,
		),
		listTargetsErr: fmt.Errorf("target list unavailable"),
	}
	canceller := &mockTaskCanceller{}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Cancel(context.Background(), runID, "operator requested", canceller)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, []string{
		"lock_run",
		"update_run",
		"list_targets",
	}, store.events)
	require.Equal(t, operationrun.OperationRunStatusCancelled, store.updatedRun.Status)
	require.Empty(t, canceller.cancelledTaskIDs)
}

func TestCancelCleanupUsesDetachedTimeoutContext(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	taskID := uuid.MustParse("22222222-2222-2222-2222-222222222222")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusRunning,
			operationrun.OperationRunStatusReasonNone,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTargetWithTask(
				runID,
				0,
				operationrun.OperationRunTargetStatusSubmitted,
				taskID,
			),
		},
		listTargetsRequireLiveContext: true,
	}
	canceller := &mockTaskCanceller{}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	got, err := manager.Cancel(ctx, runID, "operator requested", canceller)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Equal(t, []string{
		"lock_run",
		"update_run",
		"list_targets",
	}, store.events)
	require.Equal(t, []uuid.UUID{taskID}, canceller.cancelledTaskIDs)
	require.Equal(t, []error{nil}, canceller.ctxErrs)
	require.Equal(t, []bool{true}, canceller.ctxHasDeadlines)
}

func TestCancelDoesNotCancelChildTasksWhenTransactionCommitFails(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	taskID := uuid.MustParse("22222222-2222-2222-2222-222222222222")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusRunning,
			operationrun.OperationRunStatusReasonNone,
		),
		lockedTargets: []*operationrun.OperationRunTarget{
			testOperationRunTargetWithTask(
				runID,
				0,
				operationrun.OperationRunTargetStatusSubmitted,
				taskID,
			),
		},
		txCommitErr: fmt.Errorf("commit failed"),
	}
	canceller := &mockTaskCanceller{}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Cancel(context.Background(), runID, "operator requested", canceller)

	require.Nil(t, got)
	require.ErrorContains(t, err, "commit failed")
	require.Equal(t, []string{
		"lock_run",
		"update_run",
	}, store.events)
	require.Empty(t, canceller.cancelledTaskIDs)
}

func TestCancelTerminalRunIsNoOp(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	store := &mockStore{
		lockRun: testLockedRun(
			t,
			runID,
			operationrun.OperationRunStatusCompleted,
			operationrun.OperationRunStatusReasonNone,
		),
	}
	canceller := &mockTaskCanceller{}
	manager := newTestManager(t, store, planner.New(&mockTargetLookup{}, planner.Config{}))

	got, err := manager.Cancel(context.Background(), runID, "operator requested", canceller)

	require.NoError(t, err)
	require.Same(t, store.lockRun, got)
	require.Zero(t, store.lockTargetsCalls)
	require.Zero(t, store.updateRunCalls)
	require.Equal(t, []string{"lock_run"}, store.events)
	require.Empty(t, store.updatedTargets)
	require.Empty(t, canceller.cancelledTaskIDs)
}

type mockStore struct {
	runID uuid.UUID

	txCalls                       int
	createCalls                   int
	createTargetsCalls            int
	createdRun                    *operationrun.OperationRun
	createdTargets                []*operationrun.OperationRunTarget
	getErr                        error
	listTargetsErr                error
	listTargetsRequireLiveContext bool
	txCommitErr                   error

	lockRun          *operationrun.OperationRun
	lockRunNil       bool
	lockRunErr       error
	lockedTargets    []*operationrun.OperationRunTarget
	lockTargetsCalls int
	updateRunCalls   int
	updatedRun       *operationrun.OperationRun
	updatedTargets   []*operationrun.OperationRunTarget
	events           []string
}

func newTestManager(
	t *testing.T,
	store Store,
	plan planner.Planner,
) *ManagerImpl {
	t.Helper()

	manager, err := New(store, plan)
	require.NoError(t, err)
	return manager
}

func (m *mockStore) Create(
	ctx context.Context,
	run *operationrun.OperationRun,
) (uuid.UUID, error) {
	m.createCalls++
	m.createdRun = run
	return m.runID, nil
}

func (m *mockStore) Get(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	if m.getErr != nil {
		return nil, m.getErr
	}

	return nil, fmt.Errorf("not implemented")
}

func (m *mockStore) LockOperationRun(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	m.events = append(m.events, "lock_run")
	if m.lockRunErr != nil {
		return nil, m.lockRunErr
	}
	if m.lockRunNil {
		return nil, nil
	}
	if m.lockRun == nil {
		return nil, fmt.Errorf("not implemented")
	}

	return m.lockRun, nil
}

func (m *mockStore) List(
	ctx context.Context,
	opts operationrun.ListOptions,
) ([]*operationrun.OperationRun, int32, error) {
	return nil, 0, fmt.Errorf("not implemented")
}

func (m *mockStore) CreateTargets(
	ctx context.Context,
	runID uuid.UUID,
	targets []*operationrun.OperationRunTarget,
) error {
	m.createTargetsCalls++
	m.createdTargets = make([]*operationrun.OperationRunTarget, 0, len(targets))
	for _, target := range targets {
		copied := *target
		copied.OperationRunID = runID
		copied.Status = operationrun.OperationRunTargetStatusPending
		m.createdTargets = append(m.createdTargets, &copied)
	}
	return nil
}

func (m *mockStore) ListTargets(
	ctx context.Context,
	runID uuid.UUID,
	opts operationrun.TargetListOptions,
) ([]*operationrun.OperationRunTarget, int32, error) {
	m.events = append(m.events, "list_targets")
	if m.listTargetsRequireLiveContext && ctx.Err() != nil {
		return nil, 0, ctx.Err()
	}
	if m.listTargetsErr != nil {
		return nil, 0, m.listTargetsErr
	}
	if m.lockedTargets != nil {
		return m.lockedTargets, int32(len(m.lockedTargets)), nil
	}

	return nil, 0, fmt.Errorf("not implemented")
}

func (m *mockStore) LockOperationRunTargets(
	ctx context.Context,
	runID uuid.UUID,
	phaseIndex int32,
) ([]*operationrun.OperationRunTarget, error) {
	m.events = append(m.events, "lock_targets")
	m.lockTargetsCalls++
	targets := make([]*operationrun.OperationRunTarget, 0)
	for _, target := range m.lockedTargets {
		if target.PhaseIndex == phaseIndex {
			targets = append(targets, target)
		}
	}
	return targets, nil
}

func (m *mockStore) GetTargetPhaseAggregate(
	ctx context.Context,
	runID uuid.UUID,
	currentPhaseIndex int32,
) (operationrun.TargetPhaseAggregate, error) {
	completedStats := operationrun.PhaseStats{PhaseIndex: max(currentPhaseIndex-1, 0)}
	for _, target := range m.lockedTargets {
		if target.PhaseIndex < currentPhaseIndex {
			completedStats.AddTarget(target)
		}
	}

	return operationrun.TargetPhaseAggregate{
		CompletedPhaseStats: completedStats,
	}, nil
}

func (m *mockStore) UpdateRunState(
	ctx context.Context,
	run *operationrun.OperationRun,
) error {
	m.events = append(m.events, "update_run")
	m.updateRunCalls++
	copied := *run
	m.updatedRun = &copied
	return nil
}

func (m *mockStore) UpdateTargetState(
	ctx context.Context,
	target *operationrun.OperationRunTarget,
) error {
	copied := *target
	m.updatedTargets = append(m.updatedTargets, &copied)
	return nil
}

func (m *mockStore) RunInTransaction(
	ctx context.Context,
	fn func(context.Context) error,
) error {
	m.txCalls++
	if err := fn(ctx); err != nil {
		return err
	}

	return m.txCommitErr
}

type mockTargetLookup struct {
	defaultScope []operation.RackExecutionTarget
	targetSpec   []operation.RackExecutionTarget
	priorRuns    []operation.RackExecutionTarget
}

func (m *mockTargetLookup) TargetsFromDefaultScope(
	_ context.Context,
	_ *operationrun.Operation,
	_ planner.TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	return m.defaultScope, nil
}

func (m *mockTargetLookup) TargetsFromSpec(
	_ context.Context,
	_ *operation.TargetSpec,
	_ planner.TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	return m.targetSpec, nil
}

func (m *mockTargetLookup) TargetsFromRuns(
	_ context.Context,
	_ []uuid.UUID,
	_ planner.TargetLookupOptions,
) ([]operation.RackExecutionTarget, error) {
	return m.priorRuns, nil
}

func testOperationRun(t *testing.T) *operationrun.OperationRun {
	t.Helper()

	selectorRaw, err := operationrun.MarshalConfig(&operationrun.PercentageSelector{
		Percentage: 100,
		Seed:       "selector-seed",
	})
	require.NoError(t, err)

	optionsRaw, err := operationrun.MarshalConfig(operationrun.Options{
		MaxConcurrentTargets: 1,
		SafetyPolicy: operationrun.SafetyPolicy{
			Gates: []operationrun.SafetyGate{
				&operationrun.FailureCountGate{
					Scope:                 operationrun.SafetyGateScopeCurrentPhase,
					FailureThresholdCount: 1,
				},
			},
		},
		ConflictPolicy: operationrun.ConflictPolicy{
			Payload: &operationrun.ConflictRetryPolicy{
				RetryTimeout:      time.Hour,
				InitialRetryDelay: time.Second,
				MaxRetryDelay:     time.Minute,
			},
		},
		OrderingPolicy: operationrun.OrderingPolicy{
			Payload: &operationrun.RandomOrdering{Seed: "ordering-seed"},
		},
		PhasePolicy: operationrun.PhasePolicy{
			Plan: &operationrun.EqualPhases{PhaseCount: 2},
		},
	})
	require.NoError(t, err)

	firmware := &operations.FirmwareControlTaskInfo{
		Operation: operations.FirmwareOperationUpgrade,
	}
	operationRaw, err := operationrun.MarshalConfig(operationrun.Operation{
		Type:    firmware.Type(),
		Code:    firmware.CodeString(),
		Payload: firmware,
	})
	require.NoError(t, err)

	return &operationrun.OperationRun{
		Name:              "firmware rollout",
		Selector:          selectorRaw,
		Options:           optionsRaw,
		OperationTemplate: operationRaw,
		OperationType:     firmware.Type(),
		OperationCode:     firmware.CodeString(),
	}
}

func testExecutionTargets(count int) []operation.RackExecutionTarget {
	targets := make([]operation.RackExecutionTarget, 0, count)
	for i := range count {
		id := i + 1
		targets = append(targets, operation.RackExecutionTarget{
			RackID: mustUUID(fmt.Sprintf("00000000-0000-0000-0000-%012d", id)),
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute: {
					mustUUID(fmt.Sprintf("10000000-0000-0000-0000-%012d", id)),
				},
			},
		})
	}
	return targets
}

func mustUUID(value string) uuid.UUID {
	id, err := uuid.Parse(value)
	if err != nil {
		panic(err)
	}
	return id
}

func testLockedRun(
	t *testing.T,
	id uuid.UUID,
	status operationrun.OperationRunStatus,
	reason operationrun.OperationRunStatusReason,
) *operationrun.OperationRun {
	t.Helper()

	run := testOperationRun(t)
	run.ID = id
	run.Status = status
	run.StatusReason = reason
	run.TotalPhases = 2
	return run
}

func setOperationRunSafetyPolicy(
	t *testing.T,
	run *operationrun.OperationRun,
	policy operationrun.SafetyPolicy,
) {
	t.Helper()

	options, err := run.DecodedOptions()
	require.NoError(t, err)

	options.SafetyPolicy = policy
	raw, err := operationrun.MarshalConfig(options)
	require.NoError(t, err)
	run.Options = raw
}

func testOperationRunTarget(
	runID uuid.UUID,
	phase int32,
	status operationrun.OperationRunTargetStatus,
) *operationrun.OperationRunTarget {
	return &operationrun.OperationRunTarget{
		ID:             uuid.New(),
		OperationRunID: runID,
		RackID:         uuid.New(),
		PhaseIndex:     phase,
		Status:         status,
	}
}

func testOperationRunTargetWithTask(
	runID uuid.UUID,
	phase int32,
	status operationrun.OperationRunTargetStatus,
	taskID uuid.UUID,
) *operationrun.OperationRunTarget {
	target := testOperationRunTarget(runID, phase, status)
	target.TaskID = &taskID
	return target
}

type mockTaskCanceller struct {
	cancelledTaskIDs []uuid.UUID
	ctxErrs          []error
	ctxHasDeadlines  []bool
	cancelErr        error
}

func (m *mockTaskCanceller) CancelTask(
	ctx context.Context,
	taskID uuid.UUID,
) error {
	m.cancelledTaskIDs = append(m.cancelledTaskIDs, taskID)
	m.ctxErrs = append(m.ctxErrs, ctx.Err())
	_, ok := ctx.Deadline()
	m.ctxHasDeadlines = append(m.ctxHasDeadlines, ok)
	return m.cancelErr
}

func targetPhaseIndexes(
	targets []*operationrun.OperationRunTarget,
) []int32 {
	indexes := make([]int32, 0, len(targets))
	for _, target := range targets {
		indexes = append(indexes, target.PhaseIndex)
	}
	return indexes
}

func targetSequenceIndexes(
	targets []*operationrun.OperationRunTarget,
) []int32 {
	indexes := make([]int32, 0, len(targets))
	for _, target := range targets {
		indexes = append(indexes, target.SequenceIndex)
	}
	return indexes
}
