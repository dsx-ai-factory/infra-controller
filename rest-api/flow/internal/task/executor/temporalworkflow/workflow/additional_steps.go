// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

const additionalRuleStepsChangeID = "additional-rule-steps"

// additionalRuleContinuations adapts manager-bound steps to the stage runner.
// The marker preserves histories begun by workers that ignored the new input.
func additionalRuleContinuations(ctx workflow.Context, info *task.ExecutionInfo) ([]ruleContinuation, error) {
	if len(info.AdditionalSteps) == 0 {
		return nil, nil
	}
	if workflow.GetVersion(ctx, additionalRuleStepsChangeID, workflow.DefaultVersion, 1) == workflow.DefaultVersion {
		return nil, nil
	}
	err := info.ValidateAdditionalSteps()
	if err != nil {
		return nil, err
	}
	continuations := make([]ruleContinuation, 0, len(info.AdditionalSteps))
	for _, step := range info.AdditionalSteps {
		options, err := step.ResolveOperationInfo(*info)
		if err != nil {
			return nil, err
		}
		continuations = append(continuations, ruleContinuation{
			rule: operationrules.RuleDefinition{Version: operationrules.CurrentRuleDefinitionVersion,
				Steps: []operationrules.SequenceStep{step.Definition}},
			targets:       buildTargets(&task.ExecutionInfo{TraceID: info.TraceID, Components: step.Components}),
			operationInfo: options,
		})
	}
	return continuations, nil
}
