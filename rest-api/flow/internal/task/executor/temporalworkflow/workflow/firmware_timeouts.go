// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

const firmwareTimeoutBudgetChangeID = "firmware-timeout-budget"

func firmwareTimeoutBudgetEnabled(ctx workflow.Context) bool {
	return workflow.GetVersion(ctx, firmwareTimeoutBudgetChangeID, workflow.DefaultVersion, 1) != workflow.DefaultVersion
}

func firmwarePollTimeout(action operationrules.ActionConfig) time.Duration {
	if timeout := parseDurationParam(action.Parameters[operationrules.ParamPollTimeout]); timeout > 0 {
		return timeout
	}
	return 30 * time.Minute
}

// activityRetryBudget includes all attempts and the SDK's capped exponential
// backoff. ScheduleToClose uses this same bound, including queue time.
func activityRetryBudget(step operationrules.SequenceStep) time.Duration {
	opts := buildActivityOptions(step)
	retry := opts.RetryPolicy
	attempts := max(1, int(retry.MaximumAttempts))
	interval := retry.InitialInterval
	maximumInterval := retry.MaximumInterval
	if maximumInterval == 0 {
		maximumInterval = 100 * interval
	}
	coefficient := retry.BackoffCoefficient
	if coefficient == 0 {
		coefficient = 2
	}
	budget := opts.StartToCloseTimeout * time.Duration(attempts)
	for attempt := 1; attempt < attempts; attempt++ {
		budget += min(interval, maximumInterval)
		interval = time.Duration(min(float64(interval)*coefficient, float64(maximumInterval)))
	}
	return budget
}

func firmwareStepExecutionTimeout(step operationrules.SequenceStep, componentCount int) time.Duration {
	activityBudget := activityRetryBudget(step)
	budget := step.DelayAfter + 2*time.Minute
	for _, action := range step.OrderedActions() {
		actionBudget := activityBudget
		switch action.Name {
		case operationrules.ActionFirmwareControl:
			actionBudget += firmwarePollTimeout(action)
		case operationrules.ActionSleep:
			actionBudget = parseDurationParam(action.Parameters[operationrules.ParamDuration])
		case operationrules.ActionDecommissionControl:
			// This action overrides the step options with a single attempt.
			actionBudget = 5 * time.Minute
		default:
			// Existing verification loops may finish a query and sleep after
			// their deadline. Include that final activity and poll interval.
			timeout := action.Timeout
			interval := action.PollInterval
			if timeout == 0 && action.Name == operationrules.ActionWaitBringUp {
				timeout = 15 * time.Minute
			}
			if interval == 0 && action.Name == operationrules.ActionWaitBringUp {
				interval = 30 * time.Second
			}
			if timeout == 0 && action.Name == operationrules.ActionWaitDecommissioned {
				timeout = 4 * time.Hour
			}
			actionBudget += timeout + interval
		}
		budget += actionBudget * time.Duration(actionBatchCount(action, step.MaxParallel, componentCount))
	}
	return budget
}

// ExecutionTimeout retains the task-type default as a floor. For plans with
// firmware actions, sequential stages sum their longest applicable child budget.
// Existing executions retain the timeout recorded when they were started.
func ExecutionTimeout(info *task.ExecutionInfo, operationInfo any, fallback time.Duration) time.Duration {
	if info.RuleDefinition == nil {
		return fallback
	}
	targets := buildTargets(info)
	if firmware, ok := operationInfo.(*operations.FirmwareControlTaskInfo); ok {
		targets = selectFirmwareTargets(targets, firmware.TargetVersion)
	}
	hasFirmware := false
	stages := make(map[int]time.Duration)
	for _, step := range info.RuleDefinition.Steps {
		target := targets[step.ComponentType]
		if target.Len() == 0 {
			continue
		}
		budget := childWorkflowExecutionTimeout(step, target.Len())
		if stepReportsFirmwareProgress(step) {
			hasFirmware = true
			budget = firmwareStepExecutionTimeout(step, target.Len())
		}
		stages[step.Stage] = max(stages[step.Stage], budget)
	}
	if !hasFirmware {
		return fallback
	}
	var total time.Duration
	for _, budget := range stages {
		total += budget
	}
	// Reserve the original task envelope for setup, report persistence and
	// final status updates; those activities are outside the child budgets.
	return total + fallback
}
