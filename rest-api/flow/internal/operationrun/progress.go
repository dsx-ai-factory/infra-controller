// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

// TargetPhaseSummary groups targets by the active rollout phase. The current
// phase is the lowest phase index that still has non-terminal work.
type TargetPhaseSummary struct {
	TotalPhases         int32
	CurrentPhaseTargets []*OperationRunTarget
	CompletedPhaseStats PhaseStats
	CurrentPhaseStats   PhaseStats
}

type TargetPhaseAggregate struct {
	CompletedPhaseStats PhaseStats
}

// SafetyGateEvaluation reports whether a validated safety gate blocks progress.
type SafetyGateEvaluation struct {
	Tripped bool
	Message string
}

// NewTargetPhaseSummary summarizes a persisted current phase from the current
// phase's target rows and SQL-derived aggregate stats for earlier phases.
func NewTargetPhaseSummary(
	currentPhaseIndex int32,
	totalPhases int32,
	aggregate TargetPhaseAggregate,
	currentPhaseTargets []*OperationRunTarget,
) TargetPhaseSummary {
	summary := TargetPhaseSummary{
		TotalPhases:         totalPhases,
		CurrentPhaseTargets: currentPhaseTargets,
		CompletedPhaseStats: aggregate.CompletedPhaseStats,
		CurrentPhaseStats:   PhaseStats{PhaseIndex: currentPhaseIndex},
	}

	summary.CurrentPhaseStats.AddTargets(currentPhaseTargets)

	return summary
}

// IsAllTerminal reports whether there is no remaining active target work.
func (s TargetPhaseSummary) IsAllTerminal() bool {
	return !s.HasNextPhase() && s.CurrentPhaseTerminal()
}

func (s TargetPhaseSummary) CurrentPhaseTerminal() bool {
	return s.CurrentPhaseStats.AllTargetsTerminal()
}

// CurrentPhaseEmpty reports whether the persisted phase index has no targets.
// New plans use contiguous materialized indexes, but older runs can contain
// gaps when percentage or equal phase sizes rounded to zero.
func (s TargetPhaseSummary) CurrentPhaseEmpty() bool {
	return s.CurrentPhaseStats.SelectedTargets == 0
}

func (s TargetPhaseSummary) HasNextPhase() bool {
	return s.CurrentPhaseStats.PhaseIndex+1 < s.TotalPhases
}

// TerminalRunStatus returns the terminal run status implied by an all-terminal
// target set. The boolean is false when the run still has active work or has no
// targets to summarize.
func (s TargetPhaseSummary) TerminalRunStatus() (OperationRunStatus, bool) {
	targetCount := s.SelectedTargetCount()
	if targetCount == 0 {
		return "", false
	}

	if !s.IsAllTerminal() {
		return "", false
	}

	failedOrTerminatedCount := s.FailedOrTerminatedTargetCount()
	if failedOrTerminatedCount == targetCount {
		return OperationRunStatusFailed, true
	}

	if failedOrTerminatedCount > 0 {
		return OperationRunStatusCompletedWithFailures, true
	}

	return OperationRunStatusCompleted, true
}

// CurrentPhaseNotStarted reports whether the current phase still has only
// untouched pending targets.
func (s TargetPhaseSummary) CurrentPhaseNotStarted() bool {
	if len(s.CurrentPhaseTargets) == 0 {
		return false
	}

	for _, target := range s.CurrentPhaseTargets {
		if target == nil ||
			target.Status != OperationRunTargetStatusPending ||
			target.TaskID != nil {
			return false
		}
	}

	return true
}

// StatsForSafetyScope aggregates target outcomes over the safety-gate scope
// selected by the user.
func (s TargetPhaseSummary) StatsForSafetyScope(scope SafetyGateScope) PhaseStats {
	stats := s.CurrentPhaseStats
	if scope == SafetyGateScopeCumulativeRun {
		stats.Add(s.CompletedPhaseStats)
	}
	return stats
}

func (s TargetPhaseSummary) SelectedTargetCount() int {
	return s.CompletedPhaseStats.SelectedTargets +
		s.CurrentPhaseStats.SelectedTargets
}

func (s TargetPhaseSummary) FailedOrTerminatedTargetCount() int {
	return s.CompletedPhaseStats.FailedOrTerminatedTargets() +
		s.CurrentPhaseStats.FailedOrTerminatedTargets()
}

// EvaluateSafetyGates checks validated safety gates against the target summary.
func (s TargetPhaseSummary) EvaluateSafetyGates(
	gates []SafetyGate,
) SafetyGateEvaluation {
	for _, gate := range gates {
		stats := s.StatsForSafetyScope(gate.SafetyGateScope())
		if !gate.IsTripped(stats.StatusCounts.Failed, stats.SelectedTargets) {
			continue
		}

		return SafetyGateEvaluation{
			Tripped: true,
			Message: stats.SafetyGateTrippedMessage(gate),
		}
	}

	return SafetyGateEvaluation{}
}
