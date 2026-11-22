// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"fmt"
	"time"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func init() {
	registerTaskWorkflow[operations.ComponentResetTaskInfo](taskcommon.TaskTypeComponentReset, "ComponentReset", componentReset)
}

// componentReset uses fixed execution semantics: no rule may retry a reset.
// Separate per-device activities let accepted peers finish after another fails.
func componentReset(ctx workflow.Context, info task.ExecutionInfo, payload *operations.ComponentResetTaskInfo) error {
	observeActivity := activity.NameNVOSResetCompleted
	if payload.Target == "sdn" {
		observeActivity = activity.NameSDNResetCompleted
	}
	statusCtx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: 30 * time.Second, RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 3}})
	err := updateRunningTaskStatus(statusCtx, info.TaskID)
	if err != nil {
		return err
	}
	targets := buildTargets(&info)
	target, ok := targets[devicetypes.ComponentTypeNVSwitch]
	if !ok || len(targets) != 1 || target.UsesMACAddresses() {
		return updateFinishedTaskStatus(statusCtx, info.TaskID, fmt.Errorf("factory reset requires ingested switches"), nil)
	}
	deadline := workflow.Now(ctx).Add(50 * time.Minute)
	preflightCtx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 31 * time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 1},
	})
	err = executeActivityBeforeDeadline(preflightCtx, deadline, nil, activity.NamePrepareFactoryReset, target)
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if err != nil {
		return updateFinishedTaskStatus(statusCtx, info.TaskID, fmt.Errorf("factory reset preflight: %w", err), nil)
	}
	step := report.StepRecord{ComponentType: "NVSwitch", Status: report.StatusRunning, TotalComponents: target.Len()}
	rep := &report.Report{Version: report.Version, Stages: []report.StageRecord{{Number: 1, Status: report.StatusRunning, Steps: []report.StepRecord{step}}}}
	var failures []error
	notSubmitted := 0
	pending := make([]common.Target, 0, target.Len())
	// Submit every device before observing completion, so a slow first reset
	// cannot prevent healthy peers from starting. Each mutation has one attempt.
	for index, id := range target.Identifiers {
		single := common.Target{TraceID: target.TraceID, Type: target.Type, IdentifierType: target.IdentifierType, Identifiers: []string{id}}
		remaining := deadline.Sub(workflow.Now(ctx))
		if remaining <= 0 {
			failures = append(failures, fmt.Errorf("switch %s: reset not submitted before deadline", id))
			rep.Stages[0].Steps[0].FailedComponents++
			continue
		}
		submitCtx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: min(remaining, 31*time.Minute), RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
		err = executeActivityBeforeDeadline(submitCtx, deadline, nil, activity.NameSubmitFactoryReset, single, payload.Target)
		if ctx.Err() != nil {
			return ctx.Err()
		}
		if err != nil {
			failures = append(failures, fmt.Errorf("switch %s: %w", id, err))
			var appErr *temporal.ApplicationError
			if errors.As(err, &appErr) && appErr.Type() == "FactoryResetNotReady" {
				notSubmitted = target.Len() - index
				break
			}
			if resetFailureIsDefinitive(err) {
				rep.Stages[0].Steps[0].FailedComponents++
			}
		} else {
			pending = append(pending, single)
		}
	}
	// Round-robin observation avoids repeatedly polling one pending device while
	// another device's terminal result is already available.
	for len(pending) > 0 {
		next := make([]common.Target, 0, len(pending))
		for _, single := range pending {
			remaining := deadline.Sub(workflow.Now(ctx))
			var completed bool
			if remaining <= 0 {
				err = fmt.Errorf("factory reset observation timed out; outcome is unknown")
			} else {
				pollCtx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: min(remaining, time.Minute), RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
				err = executeActivityBeforeDeadline(pollCtx, deadline, &completed, observeActivity, single)
			}
			if ctx.Err() != nil {
				return ctx.Err()
			}
			if err != nil && !resetFailureIsDefinitive(err) && deadline.After(workflow.Now(ctx)) {
				// Observation failure is not a device outcome. Retain ownership
				// and retry this read on a later round, never the reset itself.
				workflow.GetLogger(ctx).Warn("factory reset observation unavailable", "switch_id", single.Identifiers[0], "error", err)
				next = append(next, single)
			} else if err != nil {
				failures = append(failures, fmt.Errorf("switch %s: %w", single.Identifiers[0], err))
				if resetFailureIsDefinitive(err) {
					rep.Stages[0].Steps[0].FailedComponents++
				}
			} else if completed {
				rep.Stages[0].Steps[0].CompletedComponents++
			} else {
				next = append(next, single)
			}
		}
		pending = next
		updateTaskReportBestEffort(statusCtx, info.TaskID, rep)
		if len(pending) > 0 {
			remaining := deadline.Sub(workflow.Now(ctx))
			if remaining > 0 {
				err = workflow.Sleep(ctx, min(10*time.Second, remaining))
				if err != nil {
					return err
				}
			}
		}
	}
	err = errors.Join(failures...)
	if notSubmitted > 0 {
		// Task messages and report errors retain only the first line. Put the
		// stop reason and count before the underlying errors, including peers.
		err = fmt.Errorf("factory reset stopped: rack readiness check failed; %d switches were not submitted: %w", notSubmitted, err)
	}
	rep.Stages[0].Status = report.StatusCompleted
	if err != nil {
		rep.Stages[0].Status = report.StatusFailed
	}
	rep.Stages[0].Steps[0].Status = rep.Stages[0].Status
	return updateFinishedTaskStatus(statusCtx, info.TaskID, err, rep)
}

// Unknown outcomes fail the task without claiming a confirmed device failure.
func resetFailureIsDefinitive(err error) bool {
	var applicationError *temporal.ApplicationError
	return errors.As(err, &applicationError) && (applicationError.Type() == "NVOSResetFailed" || applicationError.Type() == "NVOSResetRejected" || applicationError.Type() == "SDNResetFailed" || applicationError.Type() == "SDNResetRejected")
}
