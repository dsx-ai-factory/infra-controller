// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"fmt"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
)

type safetyPolicyRuntime struct {
	gates []operationrun.SafetyGate
}

func newSafetyPolicy(options *operationrun.Options) (*safetyPolicyRuntime, error) {
	gates := make([]operationrun.SafetyGate, 0, len(options.SafetyPolicy.Gates))
	for idx, gate := range options.SafetyPolicy.Gates {
		if gate == nil {
			return nil, fmt.Errorf("safety gate %d is required", idx)
		}

		if err := gate.Validate(); err != nil {
			return nil, fmt.Errorf("safety gate %d: %w", idx, err)
		}

		gates = append(gates, gate)
	}

	return &safetyPolicyRuntime{gates: gates}, nil
}

// evaluate checks the configured safety gates against either the current phase
// or the cumulative run scope.
func (p safetyPolicyRuntime) evaluate(
	summary operationrun.TargetPhaseSummary,
) pauseDecision {
	evaluation := summary.EvaluateSafetyGates(p.gates)
	if evaluation.Tripped {
		return pauseDecision{
			pause:   true,
			reason:  operationrun.OperationRunStatusReasonSafetyGate,
			message: evaluation.Message,
		}
	}

	return pauseDecision{}
}
