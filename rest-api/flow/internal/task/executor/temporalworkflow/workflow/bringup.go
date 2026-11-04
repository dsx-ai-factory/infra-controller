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

// init registers the BringUp workflow descriptor with the package registry.
func init() {
	registerTaskWorkflow[operations.BringUpTaskInfo](
		taskcommon.TaskTypeBringUp, "BringUp", bringUp,
	)
}

// bringUpActivityOptions are the default activity options for bring-up workflows.
var bringUpActivityOptions = workflow.ActivityOptions{
	StartToCloseTimeout: 20 * time.Minute,
	RetryPolicy: &temporal.RetryPolicy{
		MaximumAttempts:    3,
		InitialInterval:    5 * time.Second,
		MaximumInterval:    1 * time.Minute,
		BackoffCoefficient: 2,
	},
}

// bringUp orchestrates the rack bring-up sequence using operation rules.
// The execution sequence is driven by the RuleDefinition attached to the
// task, falling back to a hardcoded default when no custom rule exists.
func bringUp(
	ctx workflow.Context,
	reqInfo task.ExecutionInfo,
	info *operations.BringUpTaskInfo,
) error {
	// Components and operation info are validated by executeWorkflow before
	// this function is invoked — no need to re-validate here.
	ctx = workflow.WithActivityOptions(ctx, bringUpActivityOptions)

	if err := updateRunningTaskStatus(ctx, reqInfo.TaskID); err != nil {
		return err
	}

	typeToTargets := buildTargets(&reqInfo)

	report, err := executeRuleBasedOperation(
		ctx,
		reqInfo.TaskID,
		typeToTargets,
		typeToTargets,
		info,
		reqInfo.RuleDefinition,
	)

	return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err, report)
}
