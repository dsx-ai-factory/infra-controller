// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"fmt"
	"time"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

type dispatchRunAction int

const (
	dispatchRunActionStop dispatchRunAction = iota
	dispatchRunActionClaim
)

// dispatchDecision records the run transition and execution step selected after
// preparation.
type dispatchDecision struct {
	transition     dispatchRunTransition
	action         dispatchRunAction
	options        *operationrun.Options
	op             *operationrun.Operation
	conflictPolicy conflictPolicyRuntime
	targets        []*operationrun.OperationRunTarget
}

func newStopDecision(transition dispatchRunTransition) dispatchDecision {
	return dispatchDecision{
		transition: transition,
		action:     dispatchRunActionStop,
	}
}

func newClaimDecision(
	transition dispatchRunTransition,
	options *operationrun.Options,
	op *operationrun.Operation,
	conflictPolicy conflictPolicyRuntime,
	targets []*operationrun.OperationRunTarget,
) dispatchDecision {
	return dispatchDecision{
		transition:     transition,
		action:         dispatchRunActionClaim,
		options:        options,
		op:             op,
		conflictPolicy: conflictPolicy,
		targets:        targets,
	}
}

type dispatchRunTransitionKind int

const (
	dispatchRunTransitionNone dispatchRunTransitionKind = iota
	dispatchRunTransitionFail
	dispatchRunTransitionComplete
	dispatchRunTransitionCompleteWithFailures
	dispatchRunTransitionPause
	dispatchRunTransitionStart
)

type dispatchRunTransition struct {
	kind    dispatchRunTransitionKind
	reason  operationrun.OperationRunStatusReason
	message string
}

func failRunTransition(message string) dispatchRunTransition {
	return dispatchRunTransition{
		kind:    dispatchRunTransitionFail,
		message: message,
	}
}

func pauseRunTransition(
	reason operationrun.OperationRunStatusReason,
	message string,
) dispatchRunTransition {
	return dispatchRunTransition{
		kind:    dispatchRunTransitionPause,
		reason:  reason,
		message: message,
	}
}

func startRunTransition(message string) dispatchRunTransition {
	return dispatchRunTransition{
		kind:    dispatchRunTransitionStart,
		message: message,
	}
}

func (t dispatchRunTransition) apply(
	run *operationrun.OperationRun,
	now time.Time,
) {
	switch t.kind {
	case dispatchRunTransitionNone:
		return
	case dispatchRunTransitionFail:
		run.Fail(now, t.message)
	case dispatchRunTransitionComplete:
		run.Complete(now, t.message)
	case dispatchRunTransitionCompleteWithFailures:
		run.CompleteWithFailures(now, t.message)
	case dispatchRunTransitionPause:
		run.Pause(t.reason, t.message)
	case dispatchRunTransitionStart:
		run.Start(now, t.message)
	}
}

// decide applies run-level policy gates and records the transition execution
// should apply. A claim-targets decision means execution should inspect the
// current phase for available work.
func (d *Dispatcher) decide(
	prep *preparedDispatch,
	now time.Time,
) (dispatchDecision, error) {
	if prep.prepareErr != nil {
		return newStopDecision(
			failRunTransition(
				fmt.Sprintf("invalid operation run configuration: %v", prep.prepareErr),
			),
		), nil
	}

	if !prep.hasRuntimeConfiguration() {
		return newStopDecision(
			failRunTransition("invalid operation run configuration"),
		), nil
	}

	if prep.run.Status.IsTerminal() {
		return newStopDecision(dispatchRunTransition{}), nil
	}

	if prep.summary.CurrentPhaseEmpty() && prep.run.TotalPhases > 0 {
		return newStopDecision(failRunTransition(fmt.Sprintf(
			"operation run phase %d has no targets; persisted phase plan is invalid",
			prep.run.CurrentPhaseIndex,
		))), nil
	}

	if prep.summary.SelectedTargetCount() == 0 {
		return newStopDecision(failRunTransition("operation run has no targets")), nil
	}

	if status, ok := prep.summary.TerminalRunStatus(); ok {
		message := status.Message()
		if status == operationrun.OperationRunStatusFailed {
			return newStopDecision(failRunTransition(message)), nil
		}

		if status == operationrun.OperationRunStatusCompletedWithFailures {
			return newStopDecision(
				dispatchRunTransition{
					kind:    dispatchRunTransitionCompleteWithFailures,
					message: message,
				},
			), nil
		}

		return newStopDecision(
			dispatchRunTransition{
				kind:    dispatchRunTransitionComplete,
				message: message,
			},
		), nil
	}

	targets := prep.summary.CurrentPhaseTargets

	conflictDecision := prep.conflictPolicy.evaluate(targets, now)
	if conflictDecision.pause {
		return newStopDecision(
			pauseRunTransition(conflictDecision.reason, conflictDecision.message),
		), nil
	}

	safetyDecision := prep.safetyPolicy.evaluate(
		prep.summary,
	)
	if safetyDecision.pause {
		return newStopDecision(
			pauseRunTransition(safetyDecision.reason, safetyDecision.message),
		), nil
	}

	phaseDecision := prep.phasePolicy.evaluate(
		prep.summary,
	)
	switch phaseDecision.action {
	case phaseDecisionActionAdvance:
		// The next phase's targets were not locked during this transaction.
		// Persist the phase pointer first; the next pass will lock and submit them.
		prep.run.CurrentPhaseIndex++
		return newStopDecision(startRunTransition(phaseDecision.message)), nil
	case phaseDecisionActionPause:
		return newStopDecision(
			pauseRunTransition(phaseDecision.reason, phaseDecision.message),
		), nil
	case phaseDecisionActionClaim:
		return newClaimDecision(
			startRunTransition(phaseDecision.message),
			prep.options,
			prep.op,
			prep.conflictPolicy,
			targets,
		), nil
	default:
		return dispatchDecision{}, fmt.Errorf(
			"unknown phase decision action %d",
			phaseDecision.action,
		)
	}
}
