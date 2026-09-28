// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"fmt"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// nameGenericComponentStepWorkflow is the registered Temporal name for
// genericComponentStepWorkflow. Use this constant when scheduling the child
// workflow via workflow.ExecuteChildWorkflow.
const nameGenericComponentStepWorkflow = "GenericComponentStepWorkflow"

// init registers genericComponentStepWorkflow with the package registry so it
// is included in worker registration alongside all task-type workflows.
func init() {
	register(WorkflowDescriptor{
		WorkflowName: nameGenericComponentStepWorkflow,
		WorkflowFunc: genericComponentStepWorkflow,
	})
}

// genericComponentStepWorkflow is a child workflow that handles any operation
// for a single component type. It processes components in batches according to
// the step's max_parallel setting, providing isolation and independent lifecycle
// per component type.
func genericComponentStepWorkflow(
	ctx workflow.Context,
	step operationrules.SequenceStep,
	target common.Target,
	activityInfo any,
	allTargets map[devicetypes.ComponentType]common.Target,
) error {
	log.Info().
		Str("component_type", devicetypes.ComponentTypeToString(step.ComponentType)).
		Int("component_count", target.Len()).
		Int("max_parallel", step.MaxParallel).
		Msg("Component step workflow started")

	// Build activity options from step configuration
	activityOpts := buildActivityOptions(step)
	ctx = workflow.WithActivityOptions(ctx, activityOpts)

	// 1. Execute pre-operation actions
	if shouldDo, actions := step.DoPreOperations(); shouldDo {
		log.Debug().
			Int("action_count", len(actions)).
			Msg("Executing pre-operation actions")
		if err := executeActionList(ctx, actions, target, allTargets, activityInfo); err != nil {
			return fmt.Errorf("pre-operation failed: %w", err)
		}
	}

	// 2. Execute main operation
	if shouldDo, action := step.DoMainOperation(); shouldDo {
		log.Debug().
			Str("action", action.Name).
			Msg("Executing main operation action")
		if err := executeAction(ctx, action, target, allTargets, activityInfo); err != nil {
			return fmt.Errorf("main operation failed: %w", err)
		}
	} else {
		return fmt.Errorf("step has no main_operation configured")
	}

	// 3. Execute post-operation actions
	if shouldDo, actions := step.DoPostOperations(); shouldDo {
		log.Debug().
			Int("action_count", len(actions)).
			Msg("Executing post-operation actions")
		if err := executeActionList(ctx, actions, target, allTargets, activityInfo); err != nil {
			return fmt.Errorf("post-operation failed: %w", err)
		}
	}

	// Apply delay_after (legacy field, after all actions complete)
	if step.DelayAfter > 0 {
		log.Info().
			Dur("delay", step.DelayAfter).
			Str("component_type", devicetypes.ComponentTypeToString(step.ComponentType)).
			Msg("Applying delay after step (legacy)")
		if err := workflow.Sleep(ctx, step.DelayAfter); err != nil {
			return fmt.Errorf("delay_after sleep interrupted: %w", err)
		}
	}

	log.Info().
		Str("component_type", devicetypes.ComponentTypeToString(step.ComponentType)).
		Msg("Component step workflow completed successfully")

	return nil
}
