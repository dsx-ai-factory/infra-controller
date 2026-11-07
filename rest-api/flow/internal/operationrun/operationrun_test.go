// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestOperationRunStatusIsTerminalIncludesCompletedWithFailures(t *testing.T) {
	require.True(t, OperationRunStatusCompletedWithFailures.IsTerminal())
}

func TestOperationRunStatusMessage(t *testing.T) {
	tests := []struct {
		status OperationRunStatus
		want   string
	}{
		{
			status: OperationRunStatusPending,
			want:   "operation run pending",
		},
		{
			status: OperationRunStatusRunning,
			want:   "operation run running",
		},
		{
			status: OperationRunStatusPaused,
			want:   "operation run paused",
		},
		{
			status: OperationRunStatusCompleted,
			want:   "operation run completed",
		},
		{
			status: OperationRunStatusCompletedWithFailures,
			want:   "operation run completed with failed targets",
		},
		{
			status: OperationRunStatusCancelled,
			want:   "operation run cancelled",
		},
		{
			status: OperationRunStatusFailed,
			want:   "operation run failed",
		},
		{
			status: OperationRunStatus("unknown"),
		},
	}

	for _, tt := range tests {
		t.Run(string(tt.status), func(t *testing.T) {
			require.Equal(t, tt.want, tt.status.Message())
		})
	}
}

func TestOperationRunCanPause(t *testing.T) {
	tests := []struct {
		name string
		run  *OperationRun
		want bool
	}{
		{
			name: "pending",
			run:  &OperationRun{Status: OperationRunStatusPending},
			want: true,
		},
		{
			name: "running",
			run:  &OperationRun{Status: OperationRunStatusRunning},
			want: true,
		},
		{
			name: "paused",
			run:  &OperationRun{Status: OperationRunStatusPaused},
			want: true,
		},
		{
			name: "completed",
			run:  &OperationRun{Status: OperationRunStatusCompleted},
		},
		{
			name: "nil",
			run:  nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.want, tt.run.CanPause())
		})
	}
}

func TestOperationRunCanResume(t *testing.T) {
	tests := []struct {
		name string
		run  *OperationRun
		want bool
	}{
		{
			name: "operator paused",
			run: &OperationRun{
				Status:       OperationRunStatusPaused,
				StatusReason: OperationRunStatusReasonOperatorPaused,
			},
			want: true,
		},
		{
			name: "safety paused",
			run: &OperationRun{
				Status:       OperationRunStatusPaused,
				StatusReason: OperationRunStatusReasonSafetyGate,
			},
			want: true,
		},
		{
			name: "phase gate paused",
			run: &OperationRun{
				Status:       OperationRunStatusPaused,
				StatusReason: OperationRunStatusReasonPhaseGate,
			},
		},
		{
			name: "running",
			run:  &OperationRun{Status: OperationRunStatusRunning},
		},
		{
			name: "nil",
			run:  nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.want, tt.run.CanResume())
		})
	}
}

func TestOperationRunCanAdvancePhase(t *testing.T) {
	tests := []struct {
		name string
		run  *OperationRun
		want bool
	}{
		{
			name: "phase gate paused",
			run: &OperationRun{
				Status:       OperationRunStatusPaused,
				StatusReason: OperationRunStatusReasonPhaseGate,
			},
			want: true,
		},
		{
			name: "operator paused",
			run: &OperationRun{
				Status:       OperationRunStatusPaused,
				StatusReason: OperationRunStatusReasonOperatorPaused,
			},
		},
		{
			name: "running",
			run:  &OperationRun{Status: OperationRunStatusRunning},
		},
		{
			name: "nil",
			run:  nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(t, tt.want, tt.run.CanAdvancePhase())
		})
	}
}

func TestTerminalTargetStatusesMatchIsTerminal(t *testing.T) {
	terminal := map[OperationRunTargetStatus]struct{}{}
	for _, status := range TerminalTargetStatuses() {
		terminal[status] = struct{}{}
	}

	for _, status := range []OperationRunTargetStatus{
		OperationRunTargetStatusPending,
		OperationRunTargetStatusClaimed,
		OperationRunTargetStatusBlocked,
		OperationRunTargetStatusSubmitted,
		OperationRunTargetStatusCompleted,
		OperationRunTargetStatusFailed,
		OperationRunTargetStatusTerminated,
		OperationRunTargetStatusSkipped,
	} {
		_, listed := terminal[status]
		require.Equal(t, status.IsTerminal(), listed, status)
	}
}

func TestOperationRunTargetStatusFromTaskStatus(t *testing.T) {
	tests := []struct {
		name   string
		status taskcommon.TaskStatus
		want   OperationRunTargetStatus
	}{
		{
			name:   "completed",
			status: taskcommon.TaskStatusCompleted,
			want:   OperationRunTargetStatusCompleted,
		},
		{
			name:   "failed",
			status: taskcommon.TaskStatusFailed,
			want:   OperationRunTargetStatusFailed,
		},
		{
			name:   "terminated",
			status: taskcommon.TaskStatusTerminated,
			want:   OperationRunTargetStatusTerminated,
		},
		{
			name:   "non-terminal",
			status: taskcommon.TaskStatusRunning,
			want:   OperationRunTargetStatusSubmitted,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			require.Equal(
				t,
				tt.want,
				OperationRunTargetStatusFromTaskStatus(tt.status),
			)
		})
	}
}

func TestOperationRunTargetSetMessage(t *testing.T) {
	target, taskID, retryAfter, retryState := operationRunTargetWithTaskAndRetry()

	target.SetMessage("updated")

	require.Equal(t, "updated", target.Message)
	require.Equal(t, OperationRunTargetStatusBlocked, target.Status)
	require.Equal(t, taskID, *target.TaskID)
	require.Equal(t, retryAfter, *target.RetryAfter)
	require.Equal(t, retryState, target.RetryState)
}

func TestOperationRunTargetClaim(t *testing.T) {
	target, _, _, _ := operationRunTargetWithTaskAndRetry()
	leaseExpiresAt := time.Date(2026, 7, 9, 10, 0, 0, 0, time.UTC)

	target.Claim(leaseExpiresAt, "claimed")

	require.Equal(t, OperationRunTargetStatusClaimed, target.Status)
	require.Equal(t, "claimed", target.Message)
	require.Nil(t, target.TaskID)
	require.Equal(t, leaseExpiresAt, *target.RetryAfter)
	require.Nil(t, target.RetryState)
}

func TestOperationRunTargetBlock(t *testing.T) {
	target, _, _, _ := operationRunTargetWithTaskAndRetry()
	retryAfter := time.Date(2026, 7, 9, 10, 5, 0, 0, time.UTC)
	retryState := json.RawMessage(`{"conflict":"rack"}`)

	target.Block("blocked", retryAfter, retryState)

	require.Equal(t, OperationRunTargetStatusBlocked, target.Status)
	require.Equal(t, "blocked", target.Message)
	require.Nil(t, target.TaskID)
	require.Equal(t, retryAfter, *target.RetryAfter)
	require.Equal(t, retryState, target.RetryState)
}

func TestOperationRunTargetSubmit(t *testing.T) {
	target, _, _, _ := operationRunTargetWithTaskAndRetry()
	taskID := uuid.New()

	target.Submit(taskID, "submitted")

	require.Equal(t, OperationRunTargetStatusSubmitted, target.Status)
	require.Equal(t, "submitted", target.Message)
	require.Equal(t, taskID, *target.TaskID)
	requireNoRetry(t, target)
}

func TestOperationRunTargetTerminalMutatorsClearRetry(t *testing.T) {
	tests := []struct {
		name        string
		apply       func(*OperationRunTarget)
		wantStatus  OperationRunTargetStatus
		wantMessage string
	}{
		{
			name: "fail",
			apply: func(target *OperationRunTarget) {
				target.Fail("failed")
			},
			wantStatus:  OperationRunTargetStatusFailed,
			wantMessage: "failed",
		},
		{
			name: "skip",
			apply: func(target *OperationRunTarget) {
				target.Skip("skipped")
			},
			wantStatus:  OperationRunTargetStatusSkipped,
			wantMessage: "skipped",
		},
		{
			name: "terminate",
			apply: func(target *OperationRunTarget) {
				target.Terminate("terminated")
			},
			wantStatus:  OperationRunTargetStatusTerminated,
			wantMessage: "terminated",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			target, _, _, _ := operationRunTargetWithTaskAndRetry()

			tt.apply(target)

			require.Equal(t, tt.wantStatus, target.Status)
			require.Equal(t, tt.wantMessage, target.Message)
			requireNoRetry(t, target)
		})
	}
}

func operationRunTargetWithTaskAndRetry() (
	*OperationRunTarget,
	uuid.UUID,
	time.Time,
	json.RawMessage,
) {
	taskID := uuid.New()
	retryAfter := time.Date(2026, 7, 9, 9, 0, 0, 0, time.UTC)
	retryState := json.RawMessage(`{"attempt":1}`)

	return &OperationRunTarget{
		TaskID:     &taskID,
		Status:     OperationRunTargetStatusBlocked,
		Message:    "old",
		RetryAfter: &retryAfter,
		RetryState: retryState,
	}, taskID, retryAfter, retryState
}

func requireNoRetry(t *testing.T, target *OperationRunTarget) {
	t.Helper()

	require.Nil(t, target.RetryAfter)
	require.Nil(t, target.RetryState)
}
