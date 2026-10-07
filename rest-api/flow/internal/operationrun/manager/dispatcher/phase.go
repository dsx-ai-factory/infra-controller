// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"fmt"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

type phasePolicyRuntime struct {
	autoAdvance bool
}

type phaseDecisionAction int

const (
	phaseDecisionActionClaim phaseDecisionAction = iota
	phaseDecisionActionAdvance
	phaseDecisionActionPause
)

type phaseDecision struct {
	action  phaseDecisionAction
	reason  operationrun.OperationRunStatusReason
	message string
}

func newAdvancePhaseDecision(message string) phaseDecision {
	return phaseDecision{
		action:  phaseDecisionActionAdvance,
		message: message,
	}
}

func newPhasePolicy(options *operationrun.Options) (*phasePolicyRuntime, error) {
	if err := options.PhasePolicy.Validate(); err != nil {
		return nil, fmt.Errorf("phase policy: %w", err)
	}

	return &phasePolicyRuntime{
		autoAdvance: options.PhasePolicy.AdvancePolicy.AutoAdvance,
	}, nil
}

func (p phasePolicyRuntime) evaluate(
	summary operationrun.TargetPhaseSummary,
) phaseDecision {
	if !summary.CurrentPhaseTerminal() || !summary.HasNextPhase() {
		return phaseDecision{
			action: phaseDecisionActionClaim,
		}
	}

	if p.autoAdvance {
		return newAdvancePhaseDecision("advanced to next phase")
	}

	return phaseDecision{
		action:  phaseDecisionActionPause,
		reason:  operationrun.OperationRunStatusReasonPhaseGate,
		message: "waiting for phase advance",
	}
}
