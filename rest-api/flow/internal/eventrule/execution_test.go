// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

func TestNewExecution(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)

	execution, err := NewExecution(uuid.New(), "notify", &NoopPlan{Reason: "test"}, now)
	require.NoError(t, err)

	require.NotEqual(t, uuid.Nil, execution.ID)
	require.Equal(t, ExecutionStatusPending, execution.Status)
	require.Zero(t, execution.Attempts)
	require.Equal(t, now, execution.CreatedAt)
	require.Equal(t, now, execution.UpdatedAt)
	require.NoError(t, execution.Validate())
}

func TestPlannedExecutionValidate(t *testing.T) {
	tests := map[string]struct {
		planned PlannedExecution
		wantErr string
	}{
		"valid": {
			planned: PlannedExecution{ActionName: "notify", ExecutionPlan: &NoopPlan{}},
		},
		"missing action name": {
			planned: PlannedExecution{ExecutionPlan: &NoopPlan{}},
			wantErr: "event rule action name is empty",
		},
		"missing execution plan": {
			planned: PlannedExecution{ActionName: "notify"},
			wantErr: "execution plan is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.planned.Validate()

			if test.wantErr == "" {
				require.NoError(t, err)

				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestNewExecutionSkipsEmptySubmitTaskPlan(t *testing.T) {
	operationInfo := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationForcePowerOff,
	}

	info, err := operationInfo.Marshal()
	require.NoError(t, err)

	execution, err := NewExecution(
		uuid.New(),
		"power_off",
		&SubmitTaskPlan{
			Operation: operation.Wrapper{
				Type: operationInfo.Type(),
				Code: operationInfo.CodeString(),
				Info: info,
			},
			ConflictStrategy: operation.ConflictStrategyReject,
		},
		time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC),
	)
	require.NoError(t, err)

	require.Equal(t, ExecutionStatusSkipped, execution.Status)
	require.Equal(t, ExecutionReasonNoTargets, execution.Reason)
	require.Zero(t, execution.Attempts)
	require.NoError(t, execution.Validate())
}

func TestExecutionStatus_CanBeClaimed(t *testing.T) {
	tests := map[string]struct {
		status ExecutionStatus
		want   bool
	}{
		"pending":   {status: ExecutionStatusPending, want: true},
		"running":   {status: ExecutionStatusRunning, want: true},
		"deferred":  {status: ExecutionStatusDeferred, want: true},
		"completed": {status: ExecutionStatusCompleted},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, test.status.CanBeClaimed())
		})
	}
}

func TestExecution_AcquireClaim(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	tests := map[string]struct {
		updatedAt time.Time
		claimAt   time.Time
		expiresAt time.Time
		wantErr   string
		wantIs    error
	}{
		"claimed": {
			claimAt: now.Add(time.Second),
		},
		"claim time before latest update": {
			updatedAt: now.Add(2 * time.Second),
			claimAt:   now.Add(time.Second),
			wantErr:   "execution time cannot precede update time",
			wantIs:    ErrInvalidExecutionInput,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			execution, err := NewExecution(uuid.New(), "notify", &NoopPlan{}, now)
			require.NoError(t, err)
			if !test.updatedAt.IsZero() {
				execution.UpdatedAt = test.updatedAt
			}

			before := execution.Clone()
			token := uuid.New()
			expiresAt := test.expiresAt
			if expiresAt.IsZero() {
				expiresAt = test.claimAt.Add(time.Minute)
			}
			disposition, err := execution.AcquireClaim(
				"scheduler-1",
				token,
				test.claimAt,
				expiresAt,
				4,
			)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.ErrorIs(t, err, test.wantIs)
				require.Equal(t, before, *execution)

				return
			}

			require.NoError(t, err)
			require.Equal(t, ClaimAcquired, disposition)
			require.Equal(t, ExecutionStatusRunning, execution.Status)
			require.Equal(t, 1, execution.Attempts)
			require.Equal(t, token, execution.ClaimToken)
			require.Equal(t, "scheduler-1", execution.ClaimOwner)
			require.Equal(t, expiresAt, execution.ClaimExpiresAt)
			require.True(t, execution.NextAttemptAt.IsZero())
			require.Equal(t, test.claimAt, execution.UpdatedAt)
			require.NoError(t, execution.Validate())

			_, err = execution.AcquireClaim(
				"scheduler-1",
				uuid.New(),
				now.Add(2*time.Second),
				now.Add(time.Minute),
				4,
			)
			require.ErrorContains(t, err, "does not have an expired claim")
			require.ErrorIs(t, err, ErrExecutionNotClaimable)
		})
	}

	t.Run("expired running execution", func(t *testing.T) {
		testExecutionAcquireExpiredClaim(t, now)
	})

	t.Run("unclaimable status", func(t *testing.T) {
		execution, err := NewExecution(uuid.New(), "notify", &NoopPlan{}, now)
		require.NoError(t, err)
		execution.ExecutionState = CompletedExecutionResult().stateAt(now)
		execution.Attempts = 1
		before := execution.Clone()

		disposition, err := execution.AcquireClaim(
			"scheduler-1",
			uuid.New(),
			now,
			now.Add(time.Minute),
			4,
		)

		require.ErrorIs(t, err, ErrExecutionNotClaimable)
		require.Equal(t, ClaimUnspecified, disposition)
		require.Equal(t, before, *execution)
	})

	t.Run("validates input before status", func(t *testing.T) {
		execution, err := NewExecution(uuid.New(), "notify", &NoopPlan{}, now)
		require.NoError(t, err)
		execution.ExecutionState = CompletedExecutionResult().stateAt(now)
		execution.Attempts = 1

		_, err = execution.AcquireClaim(
			"",
			uuid.New(),
			now,
			now.Add(time.Minute),
			4,
		)

		require.ErrorIs(t, err, ErrInvalidExecutionInput)
		require.NotErrorIs(t, err, ErrExecutionNotClaimable)
	})
}

func testExecutionAcquireExpiredClaim(t *testing.T, now time.Time) {
	t.Helper()

	execution, err := NewExecution(uuid.New(), "notify", &NoopPlan{}, now)
	require.NoError(t, err)
	oldToken := uuid.New()
	requireExecutionClaimAcquired(t, execution, "scheduler-1", oldToken, now, now.Add(time.Minute), 2)

	t.Run("rotates ownership", func(t *testing.T) {
		candidate := execution.Clone()
		newToken := uuid.New()
		claimAt := now.Add(time.Minute)
		disposition, err := candidate.AcquireClaim(
			"scheduler-2",
			newToken,
			claimAt,
			claimAt.Add(time.Minute),
			2,
		)

		require.NoError(t, err)
		require.Equal(t, ClaimAcquired, disposition)
		require.Equal(t, 2, candidate.Attempts)
		require.Equal(t, newToken, candidate.ClaimToken)
		require.Equal(t, "scheduler-2", candidate.ClaimOwner)
		require.Equal(t, claimAt.Add(2*time.Minute), candidate.ClaimExpiresAt)
		require.NoError(t, candidate.Validate())
	})

	t.Run("repeated reclamation remains bounded", func(t *testing.T) {
		candidate := execution.Clone()
		secondClaimAt := now.Add(time.Minute)
		disposition, err := candidate.AcquireClaim(
			"scheduler-2",
			uuid.New(),
			secondClaimAt,
			secondClaimAt.Add(time.Minute),
			3,
		)
		require.NoError(t, err)
		require.Equal(t, ClaimAcquired, disposition)
		require.Equal(t, secondClaimAt.Add(2*time.Minute), candidate.ClaimExpiresAt)

		thirdClaimAt := candidate.ClaimExpiresAt
		disposition, err = candidate.AcquireClaim(
			"scheduler-3",
			uuid.New(),
			thirdClaimAt,
			thirdClaimAt.Add(time.Minute),
			3,
		)
		require.NoError(t, err)
		require.Equal(t, ClaimAcquired, disposition)
		require.Equal(t, thirdClaimAt.Add(2*time.Minute), candidate.ClaimExpiresAt)
		require.Equal(t, 3, candidate.Attempts)
		require.NoError(t, candidate.Validate())
	})

	t.Run("fails exhausted execution", func(t *testing.T) {
		candidate := execution.Clone()
		disposition, err := candidate.AcquireClaim(
			"scheduler-2",
			uuid.New(),
			now.Add(time.Minute),
			now.Add(2*time.Minute),
			1,
		)

		require.NoError(t, err)
		require.Equal(t, ClaimExhausted, disposition)
		require.Equal(t, ExecutionStatusFailed, candidate.Status)
		require.Equal(t, 1, candidate.Attempts)
		require.Equal(t, uuid.Nil, candidate.ClaimToken)
		require.Empty(t, candidate.ClaimOwner)
		require.True(t, candidate.ClaimExpiresAt.IsZero())
		require.NoError(t, candidate.Validate())
	})

	t.Run("rejects active claim", func(t *testing.T) {
		candidate := execution.Clone()
		before := candidate.Clone()
		disposition, err := candidate.AcquireClaim(
			"scheduler-2",
			uuid.New(),
			now.Add(30*time.Second),
			now.Add(2*time.Minute),
			2,
		)

		require.ErrorContains(t, err, "does not have an expired claim")
		require.ErrorIs(t, err, ErrExecutionNotClaimable)
		require.Equal(t, ClaimUnspecified, disposition)
		require.Equal(t, before, candidate)
	})
}

func TestExecution_TransitionClaimedTo(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	tests := map[string]struct {
		result          ExecutionResult
		token           func(uuid.UUID) uuid.UUID
		claimAfter      time.Duration
		transitionAfter time.Duration
		want            ExecutionStatus
		wantAttempts    int
		wantErr         string
		wantIs          error
	}{
		"completed": {
			result:          CompletedExecutionResult(),
			transitionAfter: time.Second,
			want:            ExecutionStatusCompleted,
			wantAttempts:    1,
		},
		"deferred": {
			result:          DeferredExecutionResult(ExecutionReasonAttemptFailed, "temporary", time.Minute),
			transitionAfter: time.Second,
			want:            ExecutionStatusDeferred,
			wantAttempts:    1,
		},
		"interrupted": {
			result: DeferredExecutionResult(
				ExecutionReasonAttemptInterrupted,
				context.Canceled.Error(),
				time.Minute,
			),
			transitionAfter: time.Second,
			want:            ExecutionStatusDeferred,
		},
		"failed": {
			result:          FailedExecutionResult("terminal"),
			transitionAfter: time.Second,
			want:            ExecutionStatusFailed,
			wantAttempts:    1,
		},
		"stale token": {
			result:          CompletedExecutionResult(),
			token:           func(uuid.UUID) uuid.UUID { return uuid.New() },
			transitionAfter: time.Second,
			wantErr:         "execution claim lost",
			wantIs:          ErrExecutionClaimLost,
		},
		"transition time before latest update": {
			result:          CompletedExecutionResult(),
			claimAfter:      2 * time.Second,
			transitionAfter: time.Second,
			wantErr:         "execution time cannot precede update time",
			wantIs:          ErrInvalidExecutionInput,
		},
		"expired claim with current token": {
			result:          CompletedExecutionResult(),
			transitionAfter: time.Minute,
			want:            ExecutionStatusCompleted,
			wantAttempts:    1,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			execution, err := NewExecution(uuid.New(), "notify", &NoopPlan{}, now)
			require.NoError(t, err)

			claimToken := uuid.New()
			claimAt := now.Add(test.claimAfter)
			requireExecutionClaimAcquired(t, execution,
				"scheduler-1",
				claimToken,
				claimAt,
				claimAt.Add(time.Minute),
				4,
			)
			before := execution.Clone()

			transitionToken := claimToken
			if test.token != nil {
				transitionToken = test.token(claimToken)
			}

			err = execution.TransitionClaimedTo(
				transitionToken,
				test.result,
				now.Add(test.transitionAfter),
			)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.ErrorIs(t, err, test.wantIs)
				require.Equal(t, before, *execution)

				return
			}

			require.NoError(t, err)
			require.Equal(t, test.want, execution.Status)
			require.Equal(t, test.wantAttempts, execution.Attempts)
			require.Equal(t, uuid.Nil, execution.ClaimToken)
			require.Empty(t, execution.ClaimOwner)
			require.True(t, execution.ClaimExpiresAt.IsZero())
			require.NoError(t, execution.Validate())
		})
	}
}

func TestExecutionValidate(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	valid, err := NewExecution(uuid.New(), "notify", &NoopPlan{}, now)
	require.NoError(t, err)

	tests := map[string]struct {
		mutate  func(*Execution)
		wantErr string
	}{
		"valid": {},
		"missing id": {
			mutate:  func(execution *Execution) { execution.ID = uuid.Nil },
			wantErr: "execution id is required",
		},
		"missing event": {
			mutate:  func(execution *Execution) { execution.EventID = uuid.Nil },
			wantErr: "execution event id is required",
		},
		"missing action": {
			mutate:  func(execution *Execution) { execution.ActionName = "" },
			wantErr: "event rule action name is empty",
		},
		"missing plan": {
			mutate:  func(execution *Execution) { execution.Plan = nil },
			wantErr: "execution plan is required",
		},
		"pending with attempt": {
			mutate:  func(execution *Execution) { execution.Attempts = 1 },
			wantErr: "pending execution cannot have attempts",
		},
		"running without claim token": {
			mutate: func(execution *Execution) {
				execution.ExecutionState = ExecutionState{
					ExecutionStatusDetails: ExecutionStatusDetails{Status: ExecutionStatusRunning},
				}
				execution.Attempts = 1
				execution.ClaimOwner = "scheduler-1"
			},
			wantErr: "running execution requires claim token",
		},
		"running with invalid claim owner": {
			mutate: func(execution *Execution) {
				execution.ExecutionState = ExecutionState{
					ExecutionStatusDetails: ExecutionStatusDetails{Status: ExecutionStatusRunning},
				}
				execution.Attempts = 1
				execution.ClaimToken = uuid.New()
			},
			wantErr: "execution claim owner is empty",
		},
		"running without claim expiration": {
			mutate: func(execution *Execution) {
				execution.ExecutionState = ExecutionState{
					ExecutionStatusDetails: ExecutionStatusDetails{Status: ExecutionStatusRunning},
				}
				execution.Attempts = 1
				execution.ClaimToken = uuid.New()
				execution.ClaimOwner = "scheduler-1"
			},
			wantErr: "running execution claim expiration must follow update time",
		},
		"non-running with claim token": {
			mutate:  func(execution *Execution) { execution.ClaimToken = uuid.New() },
			wantErr: "pending execution cannot have claim token",
		},
		"non-running with claim owner": {
			mutate:  func(execution *Execution) { execution.ClaimOwner = "scheduler-1" },
			wantErr: "pending execution cannot have claim owner",
		},
		"non-running with claim expiration": {
			mutate:  func(execution *Execution) { execution.ClaimExpiresAt = now.Add(time.Minute) },
			wantErr: "pending execution cannot have claim expiration",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			execution := valid.Clone()
			if test.mutate != nil {
				test.mutate(&execution)
			}

			err := execution.Validate()

			if test.wantErr == "" {
				require.NoError(t, err)

				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestExecutionResultValidate(t *testing.T) {
	tests := map[string]struct {
		result  ExecutionResult
		wantErr string
	}{
		"completed": {result: CompletedExecutionResult()},
		"deferred":  {result: DeferredExecutionResult(ExecutionReasonAttemptFailed, "temporary", time.Second)},
		"failed":    {result: FailedExecutionResult("terminal")},
		"pending result": {
			result:  ExecutionResult{ExecutionStatusDetails: ExecutionStatusDetails{Status: ExecutionStatusPending}},
			wantErr: "pending is not an execution result",
		},
		"running result": {
			result:  ExecutionResult{ExecutionStatusDetails: ExecutionStatusDetails{Status: ExecutionStatusRunning}},
			wantErr: "running is not an execution result",
		},
		"skipped result": {
			result: ExecutionResult{ExecutionStatusDetails: ExecutionStatusDetails{
				Status: ExecutionStatusSkipped,
				Reason: ExecutionReasonNoTargets,
			}},
			wantErr: "skipped is not an execution result",
		},
		"negative retry": {
			result:  DeferredExecutionResult(ExecutionReasonAttemptFailed, "temporary", -time.Second),
			wantErr: "cannot be negative",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.result.Validate()

			if test.wantErr == "" {
				require.NoError(t, err)

				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func requireExecutionClaimAcquired(
	t *testing.T,
	execution *Execution,
	owner string,
	token uuid.UUID,
	claimAt time.Time,
	expiresAt time.Time,
	maxAttempts int,
) {
	t.Helper()

	disposition, err := execution.AcquireClaim(
		owner,
		token,
		claimAt,
		expiresAt,
		maxAttempts,
	)
	require.NoError(t, err)
	require.Equal(t, ClaimAcquired, disposition)
}
