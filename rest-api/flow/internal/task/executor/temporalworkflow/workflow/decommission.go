// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// init registers the Decommission workflow descriptor with the package registry.
func init() {
	registerTaskWorkflow[operations.DecommissionTaskInfo](
		taskcommon.TaskTypeDecommission, "Decommission", decommission,
	)
}

// decommissionActivityOptions are the default activity options for decommission workflows.
var decommissionActivityOptions = workflow.ActivityOptions{
	StartToCloseTimeout: 4 * time.Hour,
	RetryPolicy: &temporal.RetryPolicy{
		MaximumAttempts:    3,
		InitialInterval:    30 * time.Second,
		MaximumInterval:    5 * time.Minute,
		BackoffCoefficient: 2,
	},
}

// decommission orchestrates the rack decommission sequence using operation rules.
// The execution sequence is driven by the RuleDefinition attached to the task,
// falling back to a hardcoded default when no custom rule exists.
//
// The default rule enforces strict ordering:
//
//	Stage 1: Compute — decommission and wait
//	Stage 2: NVSwitch — decommission and wait
//	Stage 3: PowerShelf — decommission and wait
func decommission(
	ctx workflow.Context,
	reqInfo task.ExecutionInfo,
	info *operations.DecommissionTaskInfo,
) error {
	// Components and operation info are validated by executeWorkflow before
	// this function is invoked — no need to re-validate here.
	ctx = workflow.WithActivityOptions(ctx, decommissionActivityOptions)

	if err := updateRunningTaskStatus(ctx, reqInfo.TaskID); err != nil {
		return err
	}

	typeToTargets := buildTargets(&reqInfo)

	report, err := executeRuleBasedOperation(
		ctx,
		reqInfo.TaskID,
		typeToTargets,
		info,
		reqInfo.RuleDefinition,
	)

	return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, report)
}
