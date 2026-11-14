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
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/firmwareversion"
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

const firmwareLayeredComponentSelectionChangeID = "firmware-layered-component-selection"
const nvosComputeRestartChangeID = "nvos-compute-restart"

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

	continuations := firmwareRestartContinuations(&reqInfo, info, executionTargets)
	if len(continuations) > 0 && workflow.GetVersion(ctx, nvosComputeRestartChangeID, workflow.DefaultVersion, 1) == workflow.DefaultVersion {
		// An older worker may have started this history during a rolling update.
		// Preserve its command sequence even when the input has a new snapshot.
		continuations = nil
	}
	report, err := executeRuleBasedOperation(
		ctx,
		reqInfo.TaskID,
		executionTargets,
		typeToTargets,
		info,
		reqInfo.RuleDefinition,
		continuations...,
	)

	return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, report)
}

func selectFirmwareTargets(
	targets map[devicetypes.ComponentType]common.Target,
	targetVersion string,
) map[devicetypes.ComponentType]common.Target {
	selectedTargets := maps.Clone(targets)
	for componentType := range selectedTargets {
		if _, selected := firmwareversion.Extract(targetVersion, componentType); !selected {
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

// A durable snapshot is also the rollout boundary: predecessor tasks and
// histories have no restart components and retain their previous command sequence.
func firmwareRestartContinuations(
	info *task.ExecutionInfo,
	firmware *operations.FirmwareControlTaskInfo,
	selected map[devicetypes.ComponentType]common.Target,
) []ruleContinuation {
	switchTarget := selected[devicetypes.ComponentTypeNVSwitch]
	if len(info.FirmwareRestartComponents) == 0 ||
		switchTarget.Len() == 0 || info.RuleDefinition == nil {
		return nil
	}
	if !info.RuleDefinition.RequiresNVOSComputeRestart(firmware) {
		return nil
	}
	continuation := ruleContinuation{
		rule: operationrules.RuleDefinition{Version: operationrules.CurrentRuleDefinitionVersion,
			Steps: operationrules.ComputeRestartSteps()},
		targets: buildTargets(&task.ExecutionInfo{Components: info.FirmwareRestartComponents}),
	}
	return []ruleContinuation{continuation}
}
