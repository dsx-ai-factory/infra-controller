// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"context"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func newTestDispatcherAt(
	t *testing.T,
	deps Dependencies,
	cfg Config,
	now time.Time,
) *Dispatcher {
	t.Helper()
	dispatcher, err := New(deps, cfg)
	require.NoError(t, err)
	dispatcher.now = func() time.Time {
		return now
	}
	return dispatcher
}

func TestNewValidatesDependencies(t *testing.T) {
	tests := []struct {
		name    string
		deps    Dependencies
		wantErr string
	}{
		{
			name: "missing store",
			deps: Dependencies{
				TaskManager: &fakeTaskManager{},
				TaskStore:   &fakeTaskStore{},
			},
			wantErr: "operation run store is required",
		},
		{
			name: "missing task manager",
			deps: Dependencies{
				Store:     &fakeStore{},
				TaskStore: &fakeTaskStore{},
			},
			wantErr: "task manager is required",
		},
		{
			name: "missing task store",
			deps: Dependencies{
				Store:       &fakeStore{},
				TaskManager: &fakeTaskManager{},
			},
			wantErr: "task store is required",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dispatcher, err := New(tt.deps, Config{})
			require.Nil(t, dispatcher)
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestDispatchOnceReturnsErrorsAfterProcessingBatch(t *testing.T) {
	firstRunID := uuid.New()
	secondRunID := uuid.New()
	store := &dispatchOnceStore{
		ids:    []uuid.UUID{firstRunID, secondRunID},
		failID: firstRunID,
	}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: &fakeTaskManager{},
		TaskStore:   &fakeTaskStore{},
	}, Config{
		FetchBatch: 10,
	}, time.Date(2026, 6, 26, 10, 0, 0, 0, time.UTC))

	err := dispatcher.DispatchOnce(context.Background())

	require.ErrorContains(t, err, firstRunID.String())
	require.ErrorContains(t, err, "lock failed")
	require.Equal(t, []uuid.UUID{firstRunID, secondRunID}, store.lockCalls)
}

func TestDispatchRunSubmitsPendingTargetsUpToConcurrency(t *testing.T) {
	runID := uuid.New()
	firstRackID := uuid.New()
	firstComponentID := uuid.New()
	secondRackID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 0, 0, 0, time.UTC)

	store := newFakeStore(
		testRun(t, runID, 1, operationrun.PhasePolicy{}),
		[]*operationrun.OperationRunTarget{
			testTarget(runID, firstRackID, firstComponentID, 0, 0),
			testTarget(runID, secondRackID, uuid.New(), 1, 0),
		},
	)
	taskID := uuid.New()
	taskManager := &fakeTaskManager{
		results: map[uuid.UUID]submitResult{
			firstRackID: {ids: []uuid.UUID{taskID}},
		},
	}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: taskManager,
		TaskStore:   &fakeTaskStore{},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)

	require.Len(t, taskManager.requests, 1)
	require.Equal(t, firstRackID, taskManager.requests[0].RequiredRackID)
	require.Equal(
		t,
		targetIdempotencyKey(store.targets[0].ID),
		taskManager.requests[0].IdempotencyKey,
	)
	require.Equal(t, operation.ConflictStrategyReject, taskManager.requests[0].ConflictStrategy)
	require.Equal(
		t,
		[]operation.ComponentTarget{{UUID: firstComponentID}},
		taskManager.requests[0].TargetSpec.Components,
	)

	require.Equal(t, operationrun.OperationRunStatusRunning, store.run.Status)
	require.NotNil(t, store.run.StartedAt)
	require.Equal(t, now, *store.run.StartedAt)
	require.Equal(t, operationrun.OperationRunTargetStatusSubmitted, store.targets[0].Status)
	require.Equal(t, &taskID, store.targets[0].TaskID)
	require.Equal(t, operationrun.OperationRunTargetStatusPending, store.targets[1].Status)
}

func TestDispatchRunRetriesClaimedTargetWithStableIdempotencyKey(t *testing.T) {
	runID := uuid.New()
	rackID := uuid.New()
	taskID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 2, 0, 0, time.UTC)
	target := testTarget(runID, rackID, uuid.New(), 0, 0)
	target.Status = operationrun.OperationRunTargetStatusClaimed
	expired := now.Add(-time.Second)
	target.RetryAfter = &expired

	store := newFakeStore(
		testRun(t, runID, 1, operationrun.PhasePolicy{}),
		[]*operationrun.OperationRunTarget{target},
	)
	store.run.Status = operationrun.OperationRunStatusRunning
	taskManager := &fakeTaskManager{
		results: map[uuid.UUID]submitResult{
			rackID: {ids: []uuid.UUID{taskID}},
		},
	}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: taskManager,
		TaskStore:   &fakeTaskStore{},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)

	require.Len(t, taskManager.requests, 1)
	require.Equal(t, targetIdempotencyKey(target.ID), taskManager.requests[0].IdempotencyKey)
	require.Equal(t, operationrun.OperationRunTargetStatusSubmitted, store.targets[0].Status)
	require.Equal(t, &taskID, store.targets[0].TaskID)
}

func TestDispatchRunReconcilesCompletedTaskAndCompletesRun(t *testing.T) {
	runID := uuid.New()
	taskID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 5, 0, 0, time.UTC)
	target := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
	target.Status = operationrun.OperationRunTargetStatusSubmitted
	target.TaskID = &taskID

	store := newFakeStore(testRun(t, runID, 1, operationrun.PhasePolicy{}), []*operationrun.OperationRunTarget{target})
	store.run.Status = operationrun.OperationRunStatusRunning
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: &fakeTaskManager{},
		TaskStore: &fakeTaskStore{
			tasks: map[uuid.UUID]*taskdef.Task{
				taskID: {
					ID:      taskID,
					Status:  taskcommon.TaskStatusCompleted,
					Message: "Succeeded",
				},
			},
		},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)

	require.Equal(t, operationrun.OperationRunTargetStatusCompleted, store.targets[0].Status)
	require.Equal(t, operationrun.OperationRunStatusCompleted, store.run.Status)
	require.NotNil(t, store.run.FinishedAt)
	require.Equal(t, now, *store.run.FinishedAt)
}

func TestDispatchRunLocksOnlyCurrentPhaseTargets(t *testing.T) {
	runID := uuid.New()
	firstTaskID := uuid.New()
	secondTaskID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 6, 0, 0, time.UTC)
	first := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
	first.Status = operationrun.OperationRunTargetStatusSubmitted
	first.TaskID = &firstTaskID
	second := testTarget(runID, uuid.New(), uuid.New(), 0, 1)
	second.Status = operationrun.OperationRunTargetStatusSubmitted
	second.TaskID = &secondTaskID

	store := newFakeStore(
		testRun(t, runID, 2, operationrun.PhasePolicy{}),
		[]*operationrun.OperationRunTarget{first, second},
	)
	store.run.Status = operationrun.OperationRunStatusRunning
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: &fakeTaskManager{},
		TaskStore: &fakeTaskStore{
			tasks: map[uuid.UUID]*taskdef.Task{
				firstTaskID: {
					ID:     firstTaskID,
					Status: taskcommon.TaskStatusCompleted,
				},
				secondTaskID: {
					ID:     secondTaskID,
					Status: taskcommon.TaskStatusCompleted,
				},
			},
		},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)
	require.Equal(t, operationrun.OperationRunTargetStatusCompleted, first.Status)
	require.Equal(t, operationrun.OperationRunTargetStatusSubmitted, second.Status)
	require.Equal(t, operationrun.OperationRunStatusPaused, store.run.Status)
	require.Equal(t, operationrun.OperationRunStatusReasonPhaseGate, store.run.StatusReason)
}

func TestDispatchRunFailsLegacyEmptyPhase(t *testing.T) {
	tests := []struct {
		name        string
		autoAdvance bool
	}{
		{name: "automatic phase policy", autoAdvance: true},
		{name: "manual phase policy"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			runID := uuid.New()
			nextRackID := uuid.New()
			nextTaskID := uuid.New()
			now := time.Date(2026, 8, 14, 10, 0, 0, 0, time.UTC)
			completed := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
			completed.Status = operationrun.OperationRunTargetStatusCompleted
			pending := testTarget(runID, nextRackID, uuid.New(), 1, 3)

			store := newFakeStore(
				testRun(t, runID, 1, operationrun.PhasePolicy{
					AdvancePolicy: operationrun.PhaseAdvancePolicy{
						AutoAdvance: tt.autoAdvance,
					},
				}),
				[]*operationrun.OperationRunTarget{completed, pending},
			)
			store.run.Status = operationrun.OperationRunStatusRunning
			store.run.CurrentPhaseIndex = 1
			store.run.TotalPhases = 4
			taskManager := &fakeTaskManager{
				results: map[uuid.UUID]submitResult{
					nextRackID: {ids: []uuid.UUID{nextTaskID}},
				},
			}
			dispatcher := newTestDispatcherAt(t, Dependencies{
				Store:       store,
				TaskManager: taskManager,
				TaskStore:   &fakeTaskStore{},
			}, Config{FetchBatch: 10}, now)

			err := dispatcher.dispatchRun(context.Background(), runID)
			require.NoError(t, err)
			require.Equal(t, int32(1), store.run.CurrentPhaseIndex)
			require.Equal(t, operationrun.OperationRunStatusFailed, store.run.Status)
			require.Equal(
				t,
				"operation run phase 1 has no targets; persisted phase plan is invalid",
				store.run.StatusMessage,
			)
			require.Empty(t, taskManager.requests)
			require.Equal(t, operationrun.OperationRunTargetStatusPending, pending.Status)
		})
	}
}

func TestDispatchRunPausesWhenSafetyGateTrips(t *testing.T) {
	runID := uuid.New()
	taskID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 10, 0, 0, time.UTC)
	failed := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
	failed.Status = operationrun.OperationRunTargetStatusSubmitted
	failed.TaskID = &taskID
	pending := testTarget(runID, uuid.New(), uuid.New(), 1, 0)

	store := newFakeStore(
		testRunWithSafetyGate(
			t,
			runID,
			2,
			&operationrun.FailureCountGate{
				Scope:                 operationrun.SafetyGateScopeCurrentPhase,
				FailureThresholdCount: 1,
			},
			operationrun.PhasePolicy{},
		),
		[]*operationrun.OperationRunTarget{failed, pending},
	)
	store.run.Status = operationrun.OperationRunStatusRunning
	taskManager := &fakeTaskManager{}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: taskManager,
		TaskStore: &fakeTaskStore{
			tasks: map[uuid.UUID]*taskdef.Task{
				taskID: {
					ID:      taskID,
					Status:  taskcommon.TaskStatusFailed,
					Message: "Failed",
				},
			},
		},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)

	require.Empty(t, taskManager.requests)
	require.Equal(t, operationrun.OperationRunTargetStatusFailed, store.targets[0].Status)
	require.Equal(t, operationrun.OperationRunTargetStatusPending, store.targets[1].Status)
	require.Equal(t, operationrun.OperationRunStatusPaused, store.run.Status)
	require.Equal(t, operationrun.OperationRunStatusReasonSafetyGate, store.run.StatusReason)
	require.Equal(
		t,
		"failure_count safety gate tripped for current_phase: 1/2 targets failed (threshold 1)",
		store.run.StatusMessage,
	)
}

func TestDispatchRunBlocksConflictedTargetAndScansNextCandidate(t *testing.T) {
	runID := uuid.New()
	conflictedRackID := uuid.New()
	nextRackID := uuid.New()
	nextTaskID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 15, 0, 0, time.UTC)
	store := newFakeStore(
		testRun(t, runID, 1, operationrun.PhasePolicy{}),
		[]*operationrun.OperationRunTarget{
			testTarget(runID, conflictedRackID, uuid.New(), 0, 0),
			testTarget(runID, nextRackID, uuid.New(), 1, 0),
		},
	)
	taskManager := &fakeTaskManager{
		results: map[uuid.UUID]submitResult{
			conflictedRackID: {
				err: fmt.Errorf("rack %s already has a conflicting task: %w",
					conflictedRackID, taskmanager.ErrRackConflict),
			},
			nextRackID: {ids: []uuid.UUID{nextTaskID}},
		},
	}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: taskManager,
		TaskStore:   &fakeTaskStore{},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)

	require.Len(t, taskManager.requests, 2)
	require.Equal(t, operationrun.OperationRunTargetStatusBlocked, store.targets[0].Status)
	require.NotNil(t, store.targets[0].RetryAfter)
	require.Equal(t, now.Add(time.Second), *store.targets[0].RetryAfter)
	require.NotEmpty(t, store.targets[0].RetryState)
	require.Equal(t, operationrun.OperationRunTargetStatusSubmitted, store.targets[1].Status)
	require.Equal(t, &nextTaskID, store.targets[1].TaskID)
}

func TestDispatchRunStopsAfterOneConflictRetry(t *testing.T) {
	runID := uuid.New()
	firstRackID := uuid.New()
	secondRackID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 20, 0, 0, time.UTC)
	store := newFakeStore(
		testRun(t, runID, 1, operationrun.PhasePolicy{}),
		[]*operationrun.OperationRunTarget{
			testTarget(runID, firstRackID, uuid.New(), 0, 0),
			testTarget(runID, secondRackID, uuid.New(), 1, 0),
			testTarget(runID, uuid.New(), uuid.New(), 2, 0),
		},
	)
	taskManager := &fakeTaskManager{
		results: map[uuid.UUID]submitResult{
			firstRackID: {
				err: fmt.Errorf("rack %s already has a conflicting task: %w",
					firstRackID, taskmanager.ErrRackConflict),
			},
			secondRackID: {
				err: fmt.Errorf("rack %s already has a conflicting task: %w",
					secondRackID, taskmanager.ErrRackConflict),
			},
		},
	}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: taskManager,
		TaskStore:   &fakeTaskStore{},
	}, Config{
		FetchBatch: 10,
	}, now)

	err := dispatcher.dispatchRun(context.Background(), runID)
	require.NoError(t, err)

	require.Len(t, taskManager.requests, 2)
	require.Equal(t, firstRackID, taskManager.requests[0].RequiredRackID)
	require.Equal(t, secondRackID, taskManager.requests[1].RequiredRackID)
	require.Equal(t, operationrun.OperationRunTargetStatusBlocked, store.targets[0].Status)
	require.Equal(t, operationrun.OperationRunTargetStatusBlocked, store.targets[1].Status)
	require.Equal(t, operationrun.OperationRunTargetStatusPending, store.targets[2].Status)
}

func TestClaimLeasesTargetForSubmission(t *testing.T) {
	runID := uuid.New()
	rackID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 30, 0, 0, time.UTC)
	lease := 45 * time.Second
	run := testRun(t, runID, 1, operationrun.PhasePolicy{})
	op, err := run.DecodedOperation()
	require.NoError(t, err)

	target := testTarget(runID, rackID, uuid.New(), 0, 0)
	prep := newPreparedDispatch(run)
	claimed, err := claim(
		prep,
		dispatchDecision{
			options:        &operationrun.Options{MaxConcurrentTargets: 1},
			op:             op,
			targets:        []*operationrun.OperationRunTarget{target},
			conflictPolicy: retryConflictPolicy{policy: &operationrun.ConflictRetryPolicy{}},
		},
		now,
		lease,
	)
	require.NoError(t, err)

	require.Len(t, claimed.targets, 1)
	require.Equal(t, target, claimed.targets[0])
	require.Len(t, claimed.requests, 1)
	require.Equal(t, rackID, claimed.requests[0].RequiredRackID)
	require.Equal(t, operationrun.OperationRunTargetStatusClaimed, target.Status)
	require.Nil(t, target.TaskID)
	require.Equal(t, "claimed for submission", target.Message)
	require.NotNil(t, target.RetryAfter)
	require.Equal(t, now.Add(lease), *target.RetryAfter)
	require.Equal(t, target, prep.changed[target.ID])
}

func TestSummarizeClaimTargetsTreatsClaimLeaseAsRecoverableQuota(t *testing.T) {
	runID := uuid.New()
	now := time.Date(2026, 6, 26, 10, 35, 0, 0, time.UTC)
	activeTaskID := uuid.New()
	future := now.Add(time.Minute)
	past := now.Add(-time.Second)

	tests := []struct {
		name           string
		target         *operationrun.OperationRunTarget
		wantActive     int
		wantCandidates int
	}{
		{
			name: "submitted with task consumes quota",
			target: func() *operationrun.OperationRunTarget {
				target := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
				target.Status = operationrun.OperationRunTargetStatusSubmitted
				target.TaskID = &activeTaskID
				return target
			}(),
			wantActive: 1,
		},
		{
			name: "unexpired claim lease consumes quota",
			target: func() *operationrun.OperationRunTarget {
				target := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
				target.Status = operationrun.OperationRunTargetStatusClaimed
				target.RetryAfter = &future
				return target
			}(),
			wantActive: 1,
		},
		{
			name: "expired claim lease is claimable",
			target: func() *operationrun.OperationRunTarget {
				target := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
				target.Status = operationrun.OperationRunTargetStatusClaimed
				target.RetryAfter = &past
				return target
			}(),
			wantCandidates: 1,
		},
		{
			name: "legacy submitted without task is claimable",
			target: func() *operationrun.OperationRunTarget {
				target := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
				target.Status = operationrun.OperationRunTargetStatusSubmitted
				return target
			}(),
			wantCandidates: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			summary := summarizeClaimTargets(
				[]*operationrun.OperationRunTarget{tt.target},
				now,
			)
			require.Equal(t, tt.wantActive, summary.active)
			require.Len(t, summary.candidates, tt.wantCandidates)
		})
	}
}

func TestUpdateTargetAfterSubmitUsesConfiguredTimeout(t *testing.T) {
	runID := uuid.New()
	target := testTarget(runID, uuid.New(), uuid.New(), 0, 0)
	store := newFakeStore(
		testRun(t, runID, 1, operationrun.PhasePolicy{}),
		[]*operationrun.OperationRunTarget{target},
	)
	store.updateTargetState = func(ctx context.Context, _ *operationrun.OperationRunTarget) error {
		<-ctx.Done()
		return ctx.Err()
	}
	dispatcher := newTestDispatcherAt(t, Dependencies{
		Store:       store,
		TaskManager: &fakeTaskManager{},
		TaskStore:   &fakeTaskStore{},
	}, Config{
		SubmitPersistTimeout: time.Nanosecond,
	}, time.Date(2026, 6, 26, 10, 0, 0, 0, time.UTC))

	err := dispatcher.updateTargetAfterSubmit(context.Background(), target)

	require.ErrorIs(t, err, context.DeadlineExceeded)
}

func TestClaimStatusMessageIncludesSubmittedAndBlockedCounts(t *testing.T) {
	conflictPolicy := retryConflictPolicy{policy: &operationrun.ConflictRetryPolicy{}}
	tests := []struct {
		name      string
		submitted int
		blocked   int
		want      string
	}{
		{
			name: "none",
		},
		{
			name:      "one submitted",
			submitted: 1,
			want:      "submitted 1 operation run target",
		},
		{
			name:      "multiple submitted",
			submitted: 3,
			want:      "submitted 3 operation run targets",
		},
		{
			name:    "one blocked",
			blocked: 1,
			want:    "1 target waiting on rack conflicts",
		},
		{
			name:    "multiple blocked",
			blocked: 2,
			want:    "2 targets waiting on rack conflicts",
		},
		{
			name:      "submitted and blocked",
			submitted: 3,
			blocked:   2,
			want:      "submitted 3 operation run targets; 2 targets waiting on rack conflicts",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.want, claimStatusMessage(
				tt.submitted,
				tt.blocked,
				conflictPolicy,
			))
		})
	}
}

func TestDecideSetsTerminalStatusFromTargetOutcomes(t *testing.T) {
	now := time.Date(2026, 6, 26, 11, 0, 0, 0, time.UTC)
	tests := []struct {
		name     string
		statuses []operationrun.OperationRunTargetStatus
		want     operationrun.OperationRunStatus
		message  string
	}{
		{
			name: "all completed",
			statuses: []operationrun.OperationRunTargetStatus{
				operationrun.OperationRunTargetStatusCompleted,
				operationrun.OperationRunTargetStatusCompleted,
			},
			want:    operationrun.OperationRunStatusCompleted,
			message: "operation run completed",
		},
		{
			name: "mixed completed and failed",
			statuses: []operationrun.OperationRunTargetStatus{
				operationrun.OperationRunTargetStatusCompleted,
				operationrun.OperationRunTargetStatusFailed,
			},
			want:    operationrun.OperationRunStatusCompletedWithFailures,
			message: "operation run completed with failed targets",
		},
		{
			name: "all failed",
			statuses: []operationrun.OperationRunTargetStatus{
				operationrun.OperationRunTargetStatusFailed,
				operationrun.OperationRunTargetStatusTerminated,
			},
			want:    operationrun.OperationRunStatusFailed,
			message: "operation run failed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			run := &operationrun.OperationRun{
				Status: operationrun.OperationRunStatusRunning,
			}
			summary := operationrun.TargetPhaseSummary{}
			summary.CurrentPhaseStats.SelectedTargets = len(tt.statuses)
			for _, status := range tt.statuses {
				summary.CurrentPhaseStats.StatusCounts.Add(status)
			}

			decision, err := (&Dispatcher{}).decide(&preparedDispatch{
				run:            run,
				options:        &operationrun.Options{},
				op:             &operationrun.Operation{},
				conflictPolicy: retryConflictPolicy{policy: &operationrun.ConflictRetryPolicy{}},
				safetyPolicy:   &safetyPolicyRuntime{},
				phasePolicy:    &phasePolicyRuntime{},
				summary:        summary,
			}, now)
			require.NoError(t, err)
			require.Equal(t, dispatchRunActionStop, decision.action)
			decision.transition.apply(run, now)

			require.Equal(t, tt.want, run.Status)
			require.Equal(t, operationrun.OperationRunStatusReasonNone, run.StatusReason)
			require.Equal(t, tt.message, run.StatusMessage)
			require.NotNil(t, run.FinishedAt)
			require.Equal(t, now, *run.FinishedAt)
		})
	}
}

type fakeStore struct {
	run               *operationrun.OperationRun
	targets           []*operationrun.OperationRunTarget
	updateTargetState func(context.Context, *operationrun.OperationRunTarget) error
}

func newFakeStore(
	run *operationrun.OperationRun,
	targets []*operationrun.OperationRunTarget,
) *fakeStore {
	for _, target := range targets {
		if phaseCount := target.PhaseIndex + 1; phaseCount > run.TotalPhases {
			run.TotalPhases = phaseCount
		}
	}
	return &fakeStore{run: run, targets: targets}
}

func (s *fakeStore) RunInTransaction(
	ctx context.Context,
	fn func(ctx context.Context) error,
) error {
	return fn(ctx)
}

func (s *fakeStore) FetchRunnableIDs(
	ctx context.Context,
	limit int,
) ([]uuid.UUID, error) {
	return []uuid.UUID{s.run.ID}, nil
}

func (s *fakeStore) LockRunnable(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	if id != s.run.ID || s.run.Status.IsTerminal() ||
		s.run.Status == operationrun.OperationRunStatusPaused {
		return nil, nil
	}
	return s.run, nil
}

func (s *fakeStore) LockOperationRunTargets(
	ctx context.Context,
	runID uuid.UUID,
	phaseIndex int32,
) ([]*operationrun.OperationRunTarget, error) {
	targets := make([]*operationrun.OperationRunTarget, 0)
	for _, target := range s.targets {
		if target.PhaseIndex == phaseIndex {
			targets = append(targets, target)
		}
	}
	return targets, nil
}

func (s *fakeStore) GetTargetPhaseAggregate(
	ctx context.Context,
	runID uuid.UUID,
	currentPhaseIndex int32,
) (operationrun.TargetPhaseAggregate, error) {
	completedStats := operationrun.PhaseStats{PhaseIndex: max(currentPhaseIndex-1, 0)}
	for _, target := range s.targets {
		if target.PhaseIndex < currentPhaseIndex {
			completedStats.AddTarget(target)
		}
	}

	return operationrun.TargetPhaseAggregate{
		CompletedPhaseStats: completedStats,
	}, nil
}

func (s *fakeStore) UpdateRunState(
	ctx context.Context,
	run *operationrun.OperationRun,
) error {
	s.run = run
	return nil
}

func (s *fakeStore) UpdateTargetState(
	ctx context.Context,
	target *operationrun.OperationRunTarget,
) error {
	if s.updateTargetState != nil {
		return s.updateTargetState(ctx, target)
	}

	for idx := range s.targets {
		if s.targets[idx].ID == target.ID {
			s.targets[idx] = target
			return nil
		}
	}
	return fmt.Errorf("target %s not found", target.ID)
}

type dispatchOnceStore struct {
	ids       []uuid.UUID
	failID    uuid.UUID
	lockCalls []uuid.UUID
}

func (s *dispatchOnceStore) RunInTransaction(
	ctx context.Context,
	fn func(ctx context.Context) error,
) error {
	return fn(ctx)
}

func (s *dispatchOnceStore) FetchRunnableIDs(
	ctx context.Context,
	limit int,
) ([]uuid.UUID, error) {
	return s.ids, nil
}

func (s *dispatchOnceStore) LockRunnable(
	ctx context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	s.lockCalls = append(s.lockCalls, id)
	if id == s.failID {
		return nil, fmt.Errorf("lock failed")
	}
	return nil, nil
}

func (s *dispatchOnceStore) LockOperationRunTargets(
	ctx context.Context,
	runID uuid.UUID,
	phaseIndex int32,
) ([]*operationrun.OperationRunTarget, error) {
	return nil, nil
}

func (s *dispatchOnceStore) GetTargetPhaseAggregate(
	ctx context.Context,
	runID uuid.UUID,
	currentPhaseIndex int32,
) (operationrun.TargetPhaseAggregate, error) {
	return operationrun.TargetPhaseAggregate{}, nil
}

func (s *dispatchOnceStore) UpdateRunState(
	ctx context.Context,
	run *operationrun.OperationRun,
) error {
	return nil
}

func (s *dispatchOnceStore) UpdateTargetState(
	ctx context.Context,
	target *operationrun.OperationRunTarget,
) error {
	return nil
}

type submitResult struct {
	ids []uuid.UUID
	err error
}

type fakeTaskManager struct {
	results  map[uuid.UUID]submitResult
	requests []operation.Request
}

func (m *fakeTaskManager) SubmitTask(
	ctx context.Context,
	req *operation.Request,
) ([]uuid.UUID, error) {
	m.requests = append(m.requests, *req)
	if result, ok := m.results[req.RequiredRackID]; ok {
		return result.ids, result.err
	}
	return []uuid.UUID{uuid.New()}, nil
}

type fakeTaskStore struct {
	tasks map[uuid.UUID]*taskdef.Task
}

func (s *fakeTaskStore) GetTask(
	ctx context.Context,
	id uuid.UUID,
) (*taskdef.Task, error) {
	task, ok := s.tasks[id]
	if !ok {
		return nil, fmt.Errorf("task %s not found", id)
	}
	return task, nil
}

func testRun(
	t *testing.T,
	id uuid.UUID,
	maxConcurrentTargets int32,
	phasePolicy operationrun.PhasePolicy,
) *operationrun.OperationRun {
	t.Helper()
	return testRunWithSafetyGate(
		t,
		id,
		maxConcurrentTargets,
		&operationrun.FailureCountGate{
			Scope:                 operationrun.SafetyGateScopeCurrentPhase,
			FailureThresholdCount: 100,
		},
		phasePolicy,
	)
}

func testRunWithSafetyGate(
	t *testing.T,
	id uuid.UUID,
	maxConcurrentTargets int32,
	gate operationrun.SafetyGate,
	phasePolicy operationrun.PhasePolicy,
) *operationrun.OperationRun {
	t.Helper()

	if phasePolicy.Plan == nil {
		phasePolicy.Plan = &operationrun.EqualPhases{PhaseCount: 1}
	}

	optionsRaw, err := operationrun.MarshalConfig(operationrun.Options{
		MaxConcurrentTargets: maxConcurrentTargets,
		SafetyPolicy: operationrun.SafetyPolicy{
			Gates: []operationrun.SafetyGate{gate},
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
		PhasePolicy: phasePolicy,
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

	run := &operationrun.OperationRun{
		ID:                id,
		Name:              "firmware rollout",
		Status:            operationrun.OperationRunStatusPending,
		StatusReason:      operationrun.OperationRunStatusReasonNone,
		Options:           optionsRaw,
		OperationTemplate: operationRaw,
		OperationType:     firmware.Type(),
		OperationCode:     firmware.CodeString(),
	}
	return run
}

func testTarget(
	runID uuid.UUID,
	rackID uuid.UUID,
	componentID uuid.UUID,
	sequenceIndex int32,
	phaseIndex int32,
) *operationrun.OperationRunTarget {
	return &operationrun.OperationRunTarget{
		ID:             uuid.New(),
		OperationRunID: runID,
		RackID:         rackID,
		SequenceIndex:  sequenceIndex,
		PhaseIndex:     phaseIndex,
		ComponentsByType: operation.ComponentsByType{
			devicetypes.ComponentTypeCompute: {componentID},
		},
		Status: operationrun.OperationRunTargetStatusPending,
	}
}
