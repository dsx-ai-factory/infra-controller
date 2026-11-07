// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestNewTargetPhaseSummaryCombinesAggregateAndCurrentStats(t *testing.T) {
	currentSubmitted := &OperationRunTarget{
		PhaseIndex: 1,
		Status:     OperationRunTargetStatusSubmitted,
	}
	currentCompleted := &OperationRunTarget{
		PhaseIndex: 1,
		Status:     OperationRunTargetStatusCompleted,
	}
	summary := NewTargetPhaseSummary(
		1,
		4,
		TargetPhaseAggregate{
			CompletedPhaseStats: PhaseStats{
				PhaseIndex:      0,
				SelectedTargets: 2,
				StatusCounts: TargetStatusCounts{
					Completed: 1,
					Failed:    1,
				},
			},
		},
		[]*OperationRunTarget{currentSubmitted, currentCompleted},
	)

	require.False(t, summary.IsAllTerminal())
	require.True(t, summary.HasNextPhase())
	require.Equal(t, 4, summary.SelectedTargetCount())
	require.Equal(t, 1, summary.FailedOrTerminatedTargetCount())
	require.EqualValues(t, 1, summary.CurrentPhaseStats.PhaseIndex)
	require.Equal(
		t,
		[]*OperationRunTarget{currentSubmitted, currentCompleted},
		summary.CurrentPhaseTargets,
	)

	currentStats := summary.StatsForSafetyScope(SafetyGateScopeCurrentPhase)
	require.Equal(t, 2, currentStats.SelectedTargets)
	require.Equal(t, 1, currentStats.StatusCounts.Completed)
	require.Equal(t, 0, currentStats.StatusCounts.Failed)

	cumulativeStats := summary.StatsForSafetyScope(SafetyGateScopeCumulativeRun)
	require.Equal(t, 4, cumulativeStats.SelectedTargets)
	require.Equal(t, 2, cumulativeStats.StatusCounts.Completed)
	require.Equal(t, 1, cumulativeStats.StatusCounts.Failed)
}

func TestTargetPhaseSummaryTerminalRunStatus(t *testing.T) {
	tests := []struct {
		name                    string
		summary                 TargetPhaseSummary
		wantStatus              OperationRunStatus
		wantTerminalStatusFound bool
	}{
		{
			name: "still active",
			summary: TargetPhaseSummary{
				CurrentPhaseStats: PhaseStats{
					PhaseIndex:      1,
					SelectedTargets: 1,
				},
			},
		},
		{
			name:    "no targets",
			summary: TargetPhaseSummary{},
		},
		{
			name: "current phase completed with later materialized phase",
			summary: TargetPhaseSummary{
				TotalPhases: 4,
				CurrentPhaseStats: PhaseStats{
					PhaseIndex:      1,
					SelectedTargets: 1,
					StatusCounts: TargetStatusCounts{
						Completed: 1,
					},
				},
			},
		},
		{
			name: "all completed",
			summary: TargetPhaseSummary{
				CurrentPhaseStats: PhaseStats{
					SelectedTargets: 2,
					StatusCounts: TargetStatusCounts{
						Completed: 2,
					},
				},
			},
			wantStatus:              OperationRunStatusCompleted,
			wantTerminalStatusFound: true,
		},
		{
			name: "completed with failures",
			summary: TargetPhaseSummary{
				CurrentPhaseStats: PhaseStats{
					SelectedTargets: 2,
					StatusCounts: TargetStatusCounts{
						Completed: 1,
						Failed:    1,
					},
				},
			},
			wantStatus:              OperationRunStatusCompletedWithFailures,
			wantTerminalStatusFound: true,
		},
		{
			name: "all failed",
			summary: TargetPhaseSummary{
				CurrentPhaseStats: PhaseStats{
					SelectedTargets: 2,
					StatusCounts: TargetStatusCounts{
						Failed:     1,
						Terminated: 1,
					},
				},
			},
			wantStatus:              OperationRunStatusFailed,
			wantTerminalStatusFound: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotStatus, gotOK := tt.summary.TerminalRunStatus()

			require.Equal(t, tt.wantTerminalStatusFound, gotOK)
			require.Equal(t, tt.wantStatus, gotStatus)
		})
	}
}

func TestTargetPhaseSummaryCurrentPhaseNotStarted(t *testing.T) {
	tests := []struct {
		name    string
		targets []*OperationRunTarget
		want    bool
	}{
		{
			name: "empty",
		},
		{
			name: "pending targets",
			targets: []*OperationRunTarget{
				{Status: OperationRunTargetStatusPending},
				{Status: OperationRunTargetStatusPending},
			},
			want: true,
		},
		{
			name: "submitted target",
			targets: []*OperationRunTarget{
				{Status: OperationRunTargetStatusPending},
				{Status: OperationRunTargetStatusSubmitted},
			},
		},
		{
			name: "task assigned",
			targets: []*OperationRunTarget{
				func() *OperationRunTarget {
					taskID := uuid.New()
					return &OperationRunTarget{
						Status: OperationRunTargetStatusPending,
						TaskID: &taskID,
					}
				}(),
			},
		},
		{
			name: "nil target",
			targets: []*OperationRunTarget{
				nil,
				{Status: OperationRunTargetStatusPending},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			summary := TargetPhaseSummary{
				CurrentPhaseTargets: tt.targets,
			}

			require.Equal(t, tt.want, summary.CurrentPhaseNotStarted())
		})
	}
}

func TestTargetPhaseSummaryCurrentPhaseEmpty(t *testing.T) {
	tests := []struct {
		name    string
		targets []*OperationRunTarget
		want    bool
	}{
		{name: "empty", want: true},
		{
			name: "has target",
			targets: []*OperationRunTarget{
				{Status: OperationRunTargetStatusPending},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			summary := NewTargetPhaseSummary(
				1,
				2,
				TargetPhaseAggregate{},
				tt.targets,
			)

			require.Equal(t, tt.want, summary.CurrentPhaseEmpty())
		})
	}
}

func TestEvaluateSafetyGatesReturnsFirstTrippedGate(t *testing.T) {
	summary := NewTargetPhaseSummary(
		1,
		2,
		TargetPhaseAggregate{},
		[]*OperationRunTarget{
			{
				PhaseIndex: 1,
				Status:     OperationRunTargetStatusFailed,
			},
			{
				PhaseIndex: 1,
				Status:     OperationRunTargetStatusCompleted,
			},
		},
	)

	evaluation := summary.EvaluateSafetyGates(
		[]SafetyGate{
			&FailureCountGate{
				Scope:                 SafetyGateScopeCurrentPhase,
				FailureThresholdCount: 2,
			},
			&FailureRateGate{
				Scope:                   SafetyGateScopeCurrentPhase,
				FailureThresholdPercent: 50,
			},
		},
	)

	require.True(t, evaluation.Tripped)
	require.Equal(
		t,
		"failure_rate safety gate tripped for current_phase: 1/2 targets failed (50%, threshold 50%)",
		evaluation.Message,
	)
}
