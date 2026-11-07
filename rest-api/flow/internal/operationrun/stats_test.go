// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestProgressStatsTracksCurrentAndCumulativeStats(t *testing.T) {
	stats := ProgressStats{}
	stats.AddTargets([]*OperationRunTarget{
		{PhaseIndex: 0, Status: OperationRunTargetStatusCompleted},
		{PhaseIndex: 1, Status: OperationRunTargetStatusFailed},
		{PhaseIndex: 0, Status: OperationRunTargetStatusTerminated},
		{PhaseIndex: 1, Status: OperationRunTargetStatusSkipped},
		{PhaseIndex: 1, Status: OperationRunTargetStatusSubmitted},
		nil,
	})

	require.EqualValues(t, 1, stats.CurrentPhase.PhaseIndex)
	require.Equal(t, 3, stats.CurrentPhase.SelectedTargets)
	require.Equal(t, 1, stats.CurrentPhase.StatusCounts.Failed)
	require.Equal(t, 1, stats.CurrentPhase.StatusCounts.Skipped)
	require.Equal(t, 0, stats.CurrentPhase.StatusCounts.Completed)
	require.Equal(t, 0, stats.CurrentPhase.StatusCounts.Terminated)

	require.EqualValues(t, 1, stats.Cumulative.PhaseIndex)
	require.Equal(t, 5, stats.Cumulative.SelectedTargets)
	require.Equal(t, 1, stats.Cumulative.StatusCounts.Completed)
	require.Equal(t, 1, stats.Cumulative.StatusCounts.Failed)
	require.Equal(t, 1, stats.Cumulative.StatusCounts.Terminated)
	require.Equal(t, 1, stats.Cumulative.StatusCounts.Skipped)
}

func TestPhaseStatsFailurePercent(t *testing.T) {
	require.Equal(t, 0, PhaseStats{}.FailurePercent())

	stats := PhaseStats{
		SelectedTargets: 5,
		StatusCounts: TargetStatusCounts{
			Failed: 2,
		},
	}

	require.Equal(t, 40, stats.FailurePercent())
}

func TestPhaseStatsSafetyGateTrippedMessageIncludesRateThreshold(t *testing.T) {
	got := PhaseStats{
		SelectedTargets: 5,
		StatusCounts: TargetStatusCounts{
			Failed: 2,
		},
	}.SafetyGateTrippedMessage(
		&FailureRateGate{
			Scope:                   SafetyGateScopeCumulativeRun,
			FailureThresholdPercent: 25,
		},
	)

	require.Equal(
		t,
		"failure_rate safety gate tripped for cumulative_run: 2/5 targets failed (40%, threshold 25%)",
		got,
	)
}
