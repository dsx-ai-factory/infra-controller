// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/alert"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/message"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// sendAlert dispatches an alert on a best-effort basis. The call must never
// block or fail the workflow, so it runs with a background context.
func sendAlert(a alert.Alert) {
	alert.Send(context.Background(), a)
}

// updateRunningTaskStatus records the transition to TaskStatusRunning via the
// UpdateTaskStatus activity. Returns an error if taskID is nil or the activity fails.
func updateRunningTaskStatus(
	ctx workflow.Context,
	taskID uuid.UUID,
) error {
	if taskID == uuid.Nil {
		return fmt.Errorf("task ID is not specified")
	}

	arg := &taskdef.TaskStatusUpdate{
		ID:      taskID,
		Status:  taskcommon.TaskStatusRunning,
		Message: message.ForStatus(taskcommon.TaskStatusRunning),
	}

	return workflow.ExecuteActivity(ctx, activity.NameUpdateTaskStatus, arg).Get(ctx, nil)
}

// updateTaskReportBestEffort persists a report snapshot without failing the workflow.
func updateTaskReportBestEffort(
	ctx workflow.Context,
	taskID uuid.UUID,
	rep *report.Report,
) {
	if taskID == uuid.Nil || rep == nil {
		return
	}

	raw, err := rep.MarshalRaw()
	if err != nil {
		log.Warn().Err(err).Msg("failed to marshal task report")
		return
	}

	actx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 30 * time.Second,
		RetryPolicy: &temporal.RetryPolicy{
			MaximumAttempts: 1,
		},
	})

	arg := &taskdef.TaskReportUpdate{
		ID:     taskID,
		Report: raw,
	}

	derr := workflow.ExecuteActivity(actx, activity.NameUpdateTaskReport, arg).Get(ctx, nil)
	if derr != nil {
		log.Warn().Err(derr).Str("task_id", taskID.String()).Msg("failed to update task report")
	}
}

// updateFinishedTaskStatus records the terminal task status (Completed or Failed)
// via the UpdateTaskStatus activity. If both the operation error and the status
// update fail, the errors are joined. The operation error is always returned so
// the workflow reflects the correct failure cause.
func updateFinishedTaskStatus(
	ctx workflow.Context,
	taskID uuid.UUID,
	err error,
	rep *report.Report,
) error {
	if taskID == uuid.Nil {
		return fmt.Errorf("task ID is not specified")
	}

	// A failure to marshal the report must not mask err: the original
	// operation error is what determines the terminal task status. Drop
	// the report from this status update and proceed; raw stays nil so
	// the activity records the status without touching task.report.
	var raw json.RawMessage
	if rep != nil {
		// rep.Error is the canonical task-level error and the Tracker is
		// the authoritative writer (first failure wins; see
		// Tracker.FailStage). Only fill it in here if the Tracker did
		// not — e.g. a workflow that constructed a report outside the
		// rule-based path and never called FailStage.
		if err != nil && rep.Error == "" {
			rep.Error = message.ForFailure(err)
		}
		r, merr := rep.MarshalRaw()
		if merr != nil {
			log.Warn().Err(merr).
				Str("task_id", taskID.String()).
				Msg("failed to marshal task report; recording status without report")
		} else {
			raw = r
		}
	}

	var arg *taskdef.TaskStatusUpdate

	if err != nil {
		arg = &taskdef.TaskStatusUpdate{
			ID:      taskID,
			Status:  taskcommon.TaskStatusFailed,
			Message: message.ForFailure(err),
			Report:  raw,
		}
	} else {
		arg = &taskdef.TaskStatusUpdate{
			ID:      taskID,
			Status:  taskcommon.TaskStatusCompleted,
			Message: message.ForStatus(taskcommon.TaskStatusCompleted),
			Report:  raw,
		}
	}

	lerr := workflow.ExecuteActivity(ctx, activity.NameUpdateTaskStatus, arg).Get(ctx, nil)
	if lerr != nil {
		return errors.Join(err, fmt.Errorf("failed to update task status: %w", lerr))
	}

	return err
}

// buildTargets groups the components in ExecutionInfo by type, returning a map
// of ComponentType to Target. A nil info produces an empty (non-nil) map.
func buildTargets(
	info *taskdef.ExecutionInfo,
) map[devicetypes.ComponentType]common.Target {
	if info == nil {
		// This is unreachable code, but just in case, handle it anyway.
		// Returns a non-nil map to avoid nil pointer dereferences.
		return map[devicetypes.ComponentType]common.Target{}
	}

	// Choose one identifier type per component type before building the batches.
	// A mixed ingestion batch uses MACs for every member, without splitting RPCs.
	mapOnType := make(map[devicetypes.ComponentType]common.Target)
	for _, c := range info.Components {
		target := mapOnType[c.Type]
		target.Type = c.Type
		if c.ComponentID == "" {
			target.IdentifierType = common.IdentifierTypeMACAddress
		} else if target.IdentifierType == common.IdentifierTypeLegacy {
			target.IdentifierType = common.IdentifierTypeManagerID
		}
		mapOnType[c.Type] = target
	}
	for _, c := range info.Components {
		target := mapOnType[c.Type]
		if target.UsesMACAddresses() {
			target.Identifiers = append(target.Identifiers, c.MACAddress)
		} else {
			target.Identifiers = append(target.Identifiers, c.ComponentID)
		}
		mapOnType[c.Type] = target
	}

	return mapOnType
}

// componentTotalsByType returns a per-ComponentType count of targeted
// components, suitable for report.NewInitial. ComponentTypes absent from
// typeToTargets do not appear in the result; NewInitial then sees them
// as zero-count via map lookup and marks their steps skipped.
func componentTotalsByType(
	typeToTargets map[devicetypes.ComponentType]common.Target,
) map[devicetypes.ComponentType]int {
	out := make(map[devicetypes.ComponentType]int, len(typeToTargets))
	for ct, target := range typeToTargets {
		out[ct] = target.Len()
	}
	return out
}

// buildActivityOptions constructs activity options from a sequence step
func buildActivityOptions(step operationrules.SequenceStep) workflow.ActivityOptions {
	opts := workflow.ActivityOptions{
		StartToCloseTimeout: 20 * time.Minute, // Default timeout
	}

	// Override timeout if specified in step
	if step.Timeout > 0 {
		opts.StartToCloseTimeout = step.Timeout
	}

	// Set retry policy
	if step.RetryPolicy != nil {
		initialInterval := step.RetryPolicy.InitialInterval
		if initialInterval <= 0 {
			initialInterval = 1 * time.Second
		}

		retryPolicy := &temporal.RetryPolicy{
			MaximumAttempts:    int32(step.RetryPolicy.MaxAttempts),
			InitialInterval:    initialInterval,
			BackoffCoefficient: step.RetryPolicy.BackoffCoefficient,
		}

		if step.RetryPolicy.MaxInterval > 0 {
			retryPolicy.MaximumInterval = step.RetryPolicy.MaxInterval
		}

		opts.RetryPolicy = retryPolicy
	} else {
		// Default retry policy
		opts.RetryPolicy = &temporal.RetryPolicy{
			MaximumAttempts:    3,
			InitialInterval:    1 * time.Second,
			MaximumInterval:    1 * time.Minute,
			BackoffCoefficient: 2,
		}
	}

	return opts
}

// childWorkflowEntry pairs a launched child workflow future with its component
// type so that error attribution stays correct even when some steps are skipped.
type childWorkflowEntry struct {
	future        workflow.ChildWorkflowFuture
	componentType devicetypes.ComponentType
}

// childWorkflowExecutionTimeout returns a child workflow execution timeout that
// accommodates the full retry budget for activities, the pre/post operation
// durations, and a fixed scheduling buffer.
//
// The child workflow runs: pre-ops → main-op (with retries) → post-ops
// sequentially, so the budget must cover all three phases.
func childWorkflowExecutionTimeout(step operationrules.SequenceStep) time.Duration {
	base := step.Timeout
	if base == 0 {
		base = 30 * time.Minute
	}

	maxAttempts := 1
	var maxBackoff time.Duration
	if step.RetryPolicy != nil && step.RetryPolicy.MaxAttempts > 1 {
		maxAttempts = step.RetryPolicy.MaxAttempts
		if step.RetryPolicy.MaxInterval > 0 {
			maxBackoff = step.RetryPolicy.MaxInterval
		} else {
			maxBackoff = step.RetryPolicy.InitialInterval
		}
	}

	// Main operation: each attempt may take up to base, plus back-off between attempts.
	mainBudget := base*time.Duration(maxAttempts) +
		maxBackoff*time.Duration(maxAttempts-1)

	// Pre/post operation budgets: sum the declared timeouts of each action.
	// Actions without a timeout are assumed to be quick (covered by the buffer).
	var actionBudget time.Duration
	for _, a := range step.PreOperation {
		actionBudget += a.Timeout
	}
	for _, a := range step.PostOperation {
		actionBudget += a.Timeout
	}

	return mainBudget + actionBudget + 2*time.Minute
}

// executeGenericStageParallel executes all steps in a stage concurrently for any operation type.
// Each component type in the stage runs as a child workflow (cross-type parallelism).
// Within each type, components are batched according to the step's max_parallel setting.
func executeGenericStageParallel(
	ctx workflow.Context,
	steps []operationrules.SequenceStep,
	typeToTargets map[devicetypes.ComponentType]common.Target,
	activityInfo any,
) error {
	// Launch a child workflow for each component type that has targets.
	// Pair each future with its component type so error attribution is always
	// correct even when some steps are skipped (skipped steps shrink the
	// futures slice without a matching change to the steps slice).
	futures := make([]childWorkflowEntry, 0, len(steps))

	for _, step := range steps {
		target, exists := typeToTargets[step.ComponentType]
		if !exists || target.Len() == 0 {
			log.Info().
				Str("component_type", devicetypes.ComponentTypeToString(step.ComponentType)).
				Msg("Skipping step, no components of this type")
			continue
		}

		log.Info().
			Str("component_type", devicetypes.ComponentTypeToString(step.ComponentType)).
			Int("component_count", target.Len()).
			Int("max_parallel", step.MaxParallel).
			Msg("Starting component step as child workflow")

		childOptions := workflow.ChildWorkflowOptions{
			WorkflowID: fmt.Sprintf("component-step-%s-%s",
				workflow.GetInfo(ctx).WorkflowExecution.ID,
				devicetypes.ComponentTypeToString(step.ComponentType)),
			// Give the child workflow enough time to run all retry attempts.
			WorkflowExecutionTimeout: childWorkflowExecutionTimeout(step),
		}
		childCtx := workflow.WithChildOptions(ctx, childOptions)

		future := workflow.ExecuteChildWorkflow(
			childCtx,
			nameGenericComponentStepWorkflow,
			step,
			target,
			activityInfo,
			typeToTargets,
		)
		futures = append(futures, childWorkflowEntry{
			future:        future,
			componentType: step.ComponentType,
		})
	}

	// Wait for all child workflows and attribute any error to the correct type.
	for _, entry := range futures {
		if err := entry.future.Get(ctx, nil); err != nil {
			return fmt.Errorf("component type %s failed: %w",
				devicetypes.ComponentTypeToString(entry.componentType), err)
		}

		log.Info().
			Str("component_type", devicetypes.ComponentTypeToString(entry.componentType)).
			Msg("Component step completed successfully")
	}

	return nil
}

// parseDurationParam extracts a duration from a parameter value.
// Accepts time.Duration, string (e.g. "30s"), float64, or int (nanoseconds).
func parseDurationParam(val any) time.Duration {
	switch v := val.(type) {
	case time.Duration:
		return v
	case string:
		d, _ := time.ParseDuration(v)
		return d
	case float64:
		return time.Duration(v)
	case int:
		return time.Duration(v)
	default:
		return 0
	}
}

// executeRuleBasedOperation drives the rule's stages in order. At each
// stage boundary it advances the report.Tracker (begin -> complete or
// fail) and emits a best-effort report snapshot to the store. The first
// stage that fails short-circuits the loop; the partially-populated
// report is returned alongside the wrapped stage error so the caller can
// attach it to the terminal task status.
func executeRuleBasedOperation(
	ctx workflow.Context,
	taskID uuid.UUID,
	typeToTargets map[devicetypes.ComponentType]common.Target,
	operationInfo any,
	ruleDef *operationrules.RuleDefinition,
) (*report.Report, error) {
	if ruleDef == nil {
		return nil, fmt.Errorf(
			"rule definition is nil (resolver should never return nil)",
		)
	}

	if len(ruleDef.Steps) == 0 {
		return nil, fmt.Errorf("rule definition has no steps")
	}

	// The report mirrors the rule's stage layout up front: every stage
	// and every SequenceStep has a slot from the moment NewInitial
	// returns. Tracker advances those slots as the workflow progresses.
	tracker := &report.Tracker{
		Report: report.NewInitial(ruleDef, componentTotalsByType(typeToTargets)),
	}

	updateTaskReportBestEffort(ctx, taskID, tracker.Report)

	log.Info().
		Int("step_count", len(ruleDef.Steps)).
		Msg("Executing operation with rule definition")

	iter := operationrules.NewStageIterator(ruleDef)
	for stage := iter.Next(); stage != nil; stage = iter.Next() {
		tracker.BeginStage(stage.Number, workflow.Now(ctx))
		updateTaskReportBestEffort(ctx, taskID, tracker.Report)

		log.Info().
			Int("stage", stage.Number).
			Int("step_count", len(stage.Steps)).
			Msg("Executing stage")

		err := executeGenericStageParallel(
			ctx,
			stage.Steps,
			typeToTargets,
			operationInfo,
		)
		if err != nil {
			tracker.FailStage(stage.Number, err, workflow.Now(ctx))
			updateTaskReportBestEffort(ctx, taskID, tracker.Report)
			log.Error().
				Err(err).
				Int("stage", stage.Number).
				Msg("Stage execution failed")
			return tracker.Report, fmt.Errorf("stage %d failed: %w", stage.Number, err)
		}

		tracker.CompleteStage(stage.Number, workflow.Now(ctx))
		updateTaskReportBestEffort(ctx, taskID, tracker.Report)

		log.Info().
			Int("stage", stage.Number).
			Msg("Stage completed successfully")
	}

	log.Info().Msg("Rule-based operation completed successfully")
	return tracker.Report, nil
}
