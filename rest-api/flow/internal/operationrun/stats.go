// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrun

import "fmt"

// ProgressStats summarizes target outcomes for the active phase and for all
// phases included by the caller.
type ProgressStats struct {
	CurrentPhase PhaseStats
	Cumulative   PhaseStats
}

// PhaseStats summarizes target outcomes for a phase scope.
type PhaseStats struct {
	PhaseIndex      int32
	SelectedTargets int
	StatusCounts    TargetStatusCounts
}

// TargetStatusCounts counts terminal target statuses. Non-terminal target
// states still contribute to SelectedTargets, but not to any outcome count.
type TargetStatusCounts struct {
	Completed  int
	Failed     int
	Terminated int
	Skipped    int
}

func (s *ProgressStats) AddTargets(targets []*OperationRunTarget) {
	for _, target := range targets {
		s.AddTarget(target)
	}
}

// AddTarget is order-independent: whenever a higher phase appears, the current
// phase accumulator is reset to that phase, and later targets for that same
// phase are still counted. Callers only need to make sure every target is
// eventually processed.
func (s *ProgressStats) AddTarget(target *OperationRunTarget) {
	if s == nil || target == nil {
		return
	}

	if target.PhaseIndex > s.Cumulative.PhaseIndex {
		s.Cumulative.PhaseIndex = target.PhaseIndex
		s.CurrentPhase = PhaseStats{
			PhaseIndex: target.PhaseIndex,
		}
	}

	if target.PhaseIndex == s.CurrentPhase.PhaseIndex {
		s.CurrentPhase.AddTarget(target)
	}

	s.Cumulative.AddTarget(target)
}

func (s *PhaseStats) AddTargets(targets []*OperationRunTarget) {
	for _, target := range targets {
		s.AddTarget(target)
	}
}

func (s *PhaseStats) Add(other PhaseStats) {
	if s == nil {
		return
	}

	if other.PhaseIndex > s.PhaseIndex {
		s.PhaseIndex = other.PhaseIndex
	}
	s.SelectedTargets += other.SelectedTargets
	s.StatusCounts.Completed += other.StatusCounts.Completed
	s.StatusCounts.Failed += other.StatusCounts.Failed
	s.StatusCounts.Terminated += other.StatusCounts.Terminated
	s.StatusCounts.Skipped += other.StatusCounts.Skipped
}

func (s *PhaseStats) AddTarget(target *OperationRunTarget) {
	if s == nil || target == nil {
		return
	}

	if target.PhaseIndex > s.PhaseIndex {
		s.PhaseIndex = target.PhaseIndex
	}
	s.SelectedTargets++
	s.StatusCounts.Add(target.Status)
}

// FailurePercent returns the percentage of selected targets that failed.
func (s PhaseStats) FailurePercent() int {
	if s.SelectedTargets == 0 {
		return 0
	}
	return s.StatusCounts.Failed * 100 / s.SelectedTargets
}

func (s PhaseStats) TerminalTargets() int {
	return s.StatusCounts.Completed +
		s.StatusCounts.Failed +
		s.StatusCounts.Terminated +
		s.StatusCounts.Skipped
}

func (s PhaseStats) AllTargetsTerminal() bool {
	return s.SelectedTargets > 0 && s.TerminalTargets() == s.SelectedTargets
}

func (s PhaseStats) FailedOrTerminatedTargets() int {
	return s.StatusCounts.Failed + s.StatusCounts.Terminated
}

// SafetyGateTrippedMessage formats the pause message for a gate that tripped
// against these stats.
func (s PhaseStats) SafetyGateTrippedMessage(gate SafetyGate) string {
	scope := gate.SafetyGateScope()
	if scope == "" {
		scope = SafetyGateScopeCurrentPhase
	}

	switch typed := gate.(type) {
	case *FailureRateGate:
		return fmt.Sprintf(
			"%s safety gate tripped for %s: %d/%d targets failed (%d%%, threshold %d%%)",
			gate.SafetyGateKind(),
			scope,
			s.StatusCounts.Failed,
			s.SelectedTargets,
			s.FailurePercent(),
			typed.FailureThresholdPercent,
		)
	case *FailureCountGate:
		return fmt.Sprintf(
			"%s safety gate tripped for %s: %d/%d targets failed (threshold %d)",
			gate.SafetyGateKind(),
			scope,
			s.StatusCounts.Failed,
			s.SelectedTargets,
			typed.FailureThresholdCount,
		)
	default:
		return fmt.Sprintf(
			"%s safety gate tripped for %s: %d/%d targets failed",
			gate.SafetyGateKind(),
			scope,
			s.StatusCounts.Failed,
			s.SelectedTargets,
		)
	}
}

func (s *TargetStatusCounts) Add(status OperationRunTargetStatus) {
	if s == nil {
		return
	}

	switch status {
	case OperationRunTargetStatusCompleted:
		s.Completed++
	case OperationRunTargetStatusFailed:
		s.Failed++
	case OperationRunTargetStatusTerminated:
		s.Terminated++
	case OperationRunTargetStatusSkipped:
		s.Skipped++
	}
}
