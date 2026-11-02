// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"fmt"
	"time"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// init registers the InjectExpectation workflow descriptor with the package registry.
func init() {
	registerTaskWorkflow[operations.InjectExpectationTaskInfo](
		taskcommon.TaskTypeInjectExpectation, "InjectExpectation", injectExpectation,
	)
}

// injectExpectationComponentOrder defines the canonical order in which component
// types are processed by injectExpectationForAll. Every supported type must be
// listed here; types absent from a given execution are skipped automatically.
//
// TODO: This ordering is a tactical fix for workflow determinism (map iteration
// is non-deterministic in Go). The proper solution is to migrate injectExpectation
// to rule-based execution like the other workflows (bringUp, firmwareControl,
// powerControl), which would delete injectExpectationForAll entirely and let the
// RuleDefinition drive ordering and parallelism. That refactor is tracked as
// future work.
var injectExpectationComponentOrder = []devicetypes.ComponentType{
	devicetypes.ComponentTypePowerShelf,
	devicetypes.ComponentTypeNVSwitch,
	devicetypes.ComponentTypeCompute,
	devicetypes.ComponentTypeToRSwitch,
	devicetypes.ComponentTypeUMS,
	devicetypes.ComponentTypeCDU,
}

// injectExpectationActivityOptions are the default activity options for inject-expectation workflows.
var injectExpectationActivityOptions = workflow.ActivityOptions{
	StartToCloseTimeout: 10 * time.Minute,
	RetryPolicy: &temporal.RetryPolicy{
		MaximumAttempts:    3,
		InitialInterval:    5 * time.Second,
		MaximumInterval:    1 * time.Minute,
		BackoffCoefficient: 2,
	},
}

// InjectExpectation orchestrates injecting expected component configurations
// to their respective component manager services. Each component is processed
// via the InjectExpectation activity which delegates to the appropriate
// component manager.
func injectExpectation(
	ctx workflow.Context,
	reqInfo task.ExecutionInfo,
	info *operations.InjectExpectationTaskInfo,
) error {
	// Components and operation info are validated by executeWorkflow before
	// this function is invoked — no need to re-validate here.
	ctx = workflow.WithActivityOptions(ctx, injectExpectationActivityOptions)

	if err := updateRunningTaskStatus(ctx, reqInfo.TaskID); err != nil {
		return err
	}

	typeToTargets := buildTargets(&reqInfo)

	if err := injectExpectationForAll(ctx, typeToTargets, info); err != nil {
		return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, nil)
	}

	return updateFinishedTaskStatus(ctx, reqInfo.TaskID, nil, nil)
}

// injectExpectationForAll calls the InjectExpectation activity for each
// component type present in typeToTargets, in the order defined by
// injectExpectationComponentOrder. Types absent from the map are skipped.
// Sequential execution is intentional: it keeps error attribution clear.
func injectExpectationForAll(
	ctx workflow.Context,
	typeToTargets map[devicetypes.ComponentType]common.Target,
	info *operations.InjectExpectationTaskInfo,
) error {
	if err := validateInjectTypeToTargets(typeToTargets); err != nil {
		return err
	}

	for _, compType := range injectExpectationComponentOrder {
		target, exists := typeToTargets[compType]
		if !exists {
			continue
		}

		log.Info().
			Str("component_type", devicetypes.ComponentTypeToString(compType)).
			Int("count", target.Len()).
			Msg("Injecting expectations for component type")

		err := workflow.ExecuteActivity(
			ctx, activity.NameInjectExpectation, target, *info,
		).Get(ctx, nil)
		if err != nil {
			return fmt.Errorf(
				"InjectExpectation failed for %s: %w",
				devicetypes.ComponentTypeToString(compType), err,
			)
		}

		log.Info().
			Str("component_type", devicetypes.ComponentTypeToString(compType)).
			Msg("InjectExpectation completed for component type")
	}

	return nil
}

func validateInjectTypeToTargets(
	typeToTargets map[devicetypes.ComponentType]common.Target,
) error {
	targetTypes := make(map[devicetypes.ComponentType]struct{})
	for compType := range typeToTargets {
		targetTypes[compType] = struct{}{}
	}

	for _, compType := range injectExpectationComponentOrder {
		delete(targetTypes, compType)
	}

	if len(targetTypes) > 0 {
		unsupported := make([]devicetypes.ComponentType, 0, len(targetTypes))
		for compType := range targetTypes {
			unsupported = append(unsupported, compType)
		}
		return fmt.Errorf("not supported component types: %v", unsupported)
	}

	return nil
}
