// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

func TestEventActionExecutionRoundTrip(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	local := time.FixedZone("PDT", -7*60*60)
	base, err := eventrule.NewExecution(uuid.New(), "notify", &eventrule.NoopPlan{Reason: "test"}, now)
	require.NoError(t, err)

	tests := map[string]struct {
		claim       bool
		result      *eventrule.ExecutionResult
		activeClaim bool
	}{
		"pending": {},
		"running": {
			claim:       true,
			activeClaim: true,
		},
		"completed": {
			claim:  true,
			result: resultPointer(eventrule.CompletedExecutionResult()),
		},
		"deferred": {
			claim: true,
			result: resultPointer(eventrule.DeferredExecutionResult(
				eventrule.ExecutionReasonAttemptFailed,
				"temporary",
				time.Minute,
			)),
		},
		"interrupted": {
			claim: true,
			result: resultPointer(eventrule.DeferredExecutionResult(
				eventrule.ExecutionReasonAttemptInterrupted,
				"interrupted",
				time.Minute,
			)),
		},
		"failed": {
			claim:  true,
			result: resultPointer(eventrule.FailedExecutionResult("terminal")),
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			execution := base.Clone()
			token := uuid.New()

			if test.claim {
				disposition, err := execution.AcquireClaim(
					"scheduler-1",
					token,
					now.Add(time.Second),
					now.Add(time.Minute),
					4,
				)
				require.NoError(t, err)
				require.Equal(t, eventrule.ClaimAcquired, disposition)
			}
			if test.result != nil {
				require.NoError(
					t,
					execution.TransitionClaimedTo(token, *test.result, now.Add(2*time.Second)),
				)
			}

			persisted, err := EventActionExecutionTo(&execution)
			require.NoError(t, err)
			persisted.CreatedAt = persisted.CreatedAt.In(local)
			persisted.UpdatedAt = persisted.UpdatedAt.In(local)
			if persisted.ClaimExpiresAt != nil {
				claimExpiresAt := persisted.ClaimExpiresAt.In(local)
				persisted.ClaimExpiresAt = &claimExpiresAt
			}
			if persisted.NextAttemptAt != nil {
				nextAttemptAt := persisted.NextAttemptAt.In(local)
				persisted.NextAttemptAt = &nextAttemptAt
			}
			if test.activeClaim {
				require.NotNil(t, persisted.ClaimToken)
				require.Equal(t, token, *persisted.ClaimToken)
				require.NotNil(t, persisted.ClaimOwner)
				require.Equal(t, "scheduler-1", *persisted.ClaimOwner)
			} else {
				require.Nil(t, persisted.ClaimToken)
				require.Nil(t, persisted.ClaimOwner)
			}

			roundTripped, err := EventActionExecutionFrom(persisted)
			require.NoError(t, err)

			require.Equal(t, &execution, roundTripped)
			if !test.activeClaim {
				require.Equal(t, uuid.Nil, roundTripped.ClaimToken)
				require.Empty(t, roundTripped.ClaimOwner)
			}
		})
	}
}

func resultPointer(result eventrule.ExecutionResult) *eventrule.ExecutionResult {
	return &result
}

func TestEventActionExecutionFromRejectsInvalidPersistence(t *testing.T) {
	now := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	execution, err := eventrule.NewExecution(uuid.New(), "notify", &eventrule.NoopPlan{}, now)
	require.NoError(t, err)

	valid, err := EventActionExecutionTo(execution)
	require.NoError(t, err)

	tests := map[string]struct {
		persisted *dbmodel.EventActionExecution
		mutate    func(*dbmodel.EventActionExecution)
		wantNil   bool
		wantErr   string
	}{
		"nil": {wantNil: true},
		"unknown status": {
			persisted: valid,
			mutate:    func(execution *dbmodel.EventActionExecution) { execution.Status = "unknown" },
			wantErr:   "unknown execution status",
		},
		"missing event id": {
			persisted: valid,
			mutate:    func(execution *dbmodel.EventActionExecution) { execution.EventID = uuid.Nil },
			wantErr:   "execution event id is required",
		},
		"action type mismatch": {
			persisted: valid,
			mutate:    func(execution *dbmodel.EventActionExecution) { execution.ActionType = "send_alert" },
			wantErr:   "does not match plan type",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			var persisted *dbmodel.EventActionExecution
			if test.persisted != nil {
				copy := *test.persisted
				persisted = &copy
				test.mutate(persisted)
			}

			result, err := EventActionExecutionFrom(persisted)

			if test.wantErr == "" {
				require.NoError(t, err)
				require.Equal(t, test.wantNil, result == nil)
				return
			}

			require.ErrorIs(t, err, eventrule.ErrInvalidPersistedExecution)
			require.ErrorContains(t, err, test.wantErr)
			require.Nil(t, result)
		})
	}
}
