// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"maps"
	"time"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/firmwareplan"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// init registers the FirmwareControl workflow descriptor with the package registry.
func init() {
	registerTaskWorkflow[operations.FirmwareControlTaskInfo](
		taskcommon.TaskTypeFirmwareControl, "FirmwareControl", firmwareControl,
	)
}

// firmwareControlActivityOptions are the default activity options for firmware-control workflows.
var firmwareControlActivityOptions = workflow.ActivityOptions{
	StartToCloseTimeout: 5 * time.Minute,
	RetryPolicy: &temporal.RetryPolicy{
		MaximumAttempts:    3,
		InitialInterval:    5 * time.Second,
		MaximumInterval:    1 * time.Minute,
		BackoffCoefficient: 2,
	},
}

const firmwareSubTargetSelectionChangeID = "firmware-subtarget-selection"

const firmwareLayeredComponentSelectionChangeID = "firmware-layered-component-selection"

// firmwareControl orchestrates firmware updates using operation rules.
// The execution sequence is driven by the RuleDefinition attached to the
// task, falling back to a hardcoded default when no custom rule exists.
func firmwareControl(
	ctx workflow.Context,
	reqInfo task.ExecutionInfo,
	info *operations.FirmwareControlTaskInfo,
) error {
	// Components and operation info are validated by executeWorkflow before
	// this function is invoked — no need to re-validate here.
	ctx = workflow.WithActivityOptions(ctx, firmwareControlActivityOptions)

	if err := updateRunningTaskStatus(ctx, reqInfo.TaskID); err != nil {
		return err
	}

	if err := checkFirmwareUpdatePrerequisites(ctx, &reqInfo); err != nil {
		return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, nil)
	}

	typeToTargets := buildTargets(&reqInfo)
	executionTargets := typeToTargets
	// Histories without this marker retain every component's rule steps.
	selectionVersion := workflow.GetVersion(ctx, firmwareLayeredComponentSelectionChangeID, workflow.DefaultVersion, 1)
	if selectionVersion != workflow.DefaultVersion {
		executionTargets = selectFirmwareTargets(typeToTargets, info.TargetVersion)
	}

	var operationInfo any = info
	if len(info.SubTargets) > 0 && workflow.GetVersion(ctx, firmwareSubTargetSelectionChangeID, workflow.DefaultVersion, 1) != workflow.DefaultVersion {
		plan, err := firmwareplan.Build(reqInfo, *info)
		if err != nil {
			return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, nil)
		}
		executionTargets = maps.Clone(executionTargets)
		byType := make(componentOperationInfo, len(plan))
		for typ := range executionTargets {
			selected, ok := plan[typ]
			if ok {
				byType[typ] = &selected
			} else {
				delete(executionTargets, typ)
			}
		}
		operationInfo = byType
	}

	report, err := executeRuleBasedOperation(
		ctx,
		reqInfo.TaskID,
		executionTargets,
		typeToTargets,
		operationInfo,
		reqInfo.RuleDefinition,
	)

	return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, report)
}

func selectFirmwareTargets(
	targets map[devicetypes.ComponentType]common.Target,
	targetVersion string,
) map[devicetypes.ComponentType]common.Target {
	selectedTargets := maps.Clone(targets)
	for componentType := range selectedTargets {
		if _, selected := extractComponentTargetVersion(targetVersion, componentType); !selected {
			delete(selectedTargets, componentType)
		}
	}
	return selectedTargets
}

// checkFirmwareUpdatePrerequisites validates that firmware update can proceed.
// TODO: Implement actual prerequisite checks:
// - Verify all components are online/reachable
// - Validate firmware version data in database
// - Check component power states
// - Verify sufficient disk space for firmware images
// - Ensure no conflicting operations in progress
func checkFirmwareUpdatePrerequisites(_ workflow.Context, _ *task.ExecutionInfo) error {
	log.Info().Msg("Firmware update prerequisite checks: TODO - not yet implemented")
	return nil
}
