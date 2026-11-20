// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"fmt"

	"github.com/google/uuid"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// executeBestEffortOperation isolates each target's pre/main/post sequence.
// A failed or unknown target never enters a later stage. Other targets retain
// the rule's stage ordering, concurrency limits, activity retries and safety gates.
// This path is selected only by the persisted explicit policy, so predecessor
// Temporal histories continue through the unchanged Strict command sequence.
func executeBestEffortOperation(
	ctx workflow.Context,
	taskID uuid.UUID,
	targets map[devicetypes.ComponentType]common.Target,
	allTargets map[devicetypes.ComponentType]common.Target,
	info any,
	rule *operationrules.RuleDefinition,
	resolution *taskcommon.TargetResolution,
) (*report.Report, error) {
	tracker := &report.Tracker{Report: report.NewInitial(rule, componentTotalsByType(targets))}
	tracker.Report.TargetResolution = resolution
	// Key by type and identifier: identifiers can be shared across component types.
	excluded := make(map[devicetypes.ComponentType]map[string]string)
	// Rules express cross-type dependency order through stage barriers. Without
	// a finer dependency graph, a failed predecessor type cannot safely unblock
	// another type (e.g. dismantling shared power before compute decommission).
	failedTypes := make(map[devicetypes.ComponentType]string)
	var failures []error
	if len(resolution.Skipped) > 0 {
		failures = append(failures, fmt.Errorf("%d selected targets excluded at admission", len(resolution.Skipped)))
	}
	updateTaskReportBestEffort(ctx, taskID, tracker.Report)
	iter := operationrules.NewStageIterator(rule)
	for stage := iter.Next(); stage != nil; stage = iter.Next() {
		tracker.BeginStage(stage.Number, workflow.Now(ctx))
		active := make(map[devicetypes.ComponentType]common.Target, len(allTargets))
		for typ, original := range allTargets {
			current := original
			current.Identifiers = nil
			for _, id := range original.Identifiers {
				if excluded[typ][id] == "" {
					current.Identifiers = append(current.Identifiers, id)
				}
			}
			active[typ] = current
		}
		var stageErrors = make([]error, len(stage.Steps))
		remaining := len(stage.Steps)
		for index, step := range stage.Steps {
			index, step := index, step
			var dependencyFailure string
			// Traverse rule order, not map order, for deterministic diagnostics.
			for _, predecessor := range rule.Steps {
				if predecessor.Stage < stage.Number && predecessor.ComponentType != step.ComponentType && failedTypes[predecessor.ComponentType] != "" {
					dependencyFailure = failedTypes[predecessor.ComponentType]
					break
				}
			}
			if excluded[step.ComponentType] == nil {
				excluded[step.ComponentType] = make(map[string]string)
			}
			workflow.Go(ctx, func(childCtx workflow.Context) {
				defer func() { remaining-- }()
				target := targets[step.ComponentType]
				if target.Len() == 0 {
					return
				}
				if dependencyFailure != "" {
					for _, id := range target.Identifiers {
						if excluded[step.ComponentType][id] == "" {
							excluded[step.ComponentType][id] = dependencyFailure
						}
					}
				}
				stepInfo := info
				if byType, ok := info.(componentOperationInfo); ok {
					stepInfo = byType[step.ComponentType]
				}
				outcomes, err := executeBestEffortStep(childCtx, taskID, stage.Number, index, step, target, active, stepInfo, excluded[step.ComponentType])
				stageErrors[index] = err
				tracker.FinishStep(stage.Number, devicetypes.ComponentTypeToString(step.ComponentType), err, workflow.Now(ctx))
				var record *report.StepRecord
				// Stage numbers can have gaps; find the report slot by its canonical key.
				for i := range tracker.Report.Stages {
					if tracker.Report.Stages[i].Number == stage.Number {
						record = &tracker.Report.Stages[i].Steps[index]
						break
					}
				}
				record.Targets = outcomes
				allSkipped := len(outcomes) > 0
				for _, outcome := range outcomes {
					switch outcome.Status {
					case "completed":
						record.CompletedComponents++
					case "failed":
						record.FailedComponents++
					}
					allSkipped = allSkipped && outcome.Status == "skipped"
				}
				if allSkipped {
					record.Status = report.StatusSkipped
				}
				updateTaskReportBestEffort(childCtx, taskID, tracker.Report)
			})
		}
		waitErr := workflow.Await(ctx, func() bool { return remaining == 0 })
		if waitErr != nil {
			return tracker.Report, waitErr
		}
		for i, step := range stage.Steps {
			if stageErrors[i] != nil || len(excluded[step.ComponentType]) > 0 {
				failedTypes[step.ComponentType] = fmt.Sprintf("dependency in stage %d failed or has an unknown result", stage.Number)
			}
			for _, missing := range resolution.Skipped {
				if missing.ComponentType == step.ComponentType {
					failedTypes[step.ComponentType] = fmt.Sprintf("dependency in stage %d has missing targets", stage.Number)
				}
			}
		}
		stageErr := errors.Join(stageErrors...)
		if stageErr != nil {
			tracker.FailStage(stage.Number, stageErr, workflow.Now(ctx))
			failures = append(failures, fmt.Errorf("stage %d: %w", stage.Number, stageErr))
		} else {
			tracker.CompleteStage(stage.Number, workflow.Now(ctx))
		}
		updateTaskReportBestEffort(ctx, taskID, tracker.Report)
	}
	return tracker.Report, errors.Join(failures...)
}

// executeBestEffortStep retains step-wide coordination and group validation.
// Only component-scoped actions are split into independently owned children.
func executeBestEffortStep(
	ctx workflow.Context,
	taskID uuid.UUID,
	stage, stepIndex int,
	step operationrules.SequenceStep,
	target common.Target,
	allTargets map[devicetypes.ComponentType]common.Target,
	info any,
	excluded map[string]string,
) ([]report.TargetRecord, error) {
	outcomes := make([]report.TargetRecord, target.Len())
	kind := target.IdentifierType
	if kind == common.IdentifierTypeLegacy {
		kind = common.IdentifierTypeManagerID
	}
	for i, id := range target.Identifiers {
		outcomes[i] = report.TargetRecord{Identifier: id, IdentifierType: string(kind), Status: "completed"}
		if reason := excluded[id]; reason != "" {
			outcomes[i].Status, outcomes[i].Error = "skipped", reason
		}
	}
	var failures []error
	for actionIndex, action := range step.OrderedActions() {
		var indexes []int
		for i, outcome := range outcomes {
			if outcome.Status == "completed" {
				indexes = append(indexes, i)
			}
		}
		if len(indexes) == 0 {
			break
		}
		definition, known := actionExecutorRegistry[action.Name]
		if !known {
			return outcomes, fmt.Errorf("unknown action %q", action.Name)
		}
		// Each group action runs exactly once over all remaining targets. For
		// per-target actions, MaxParallel bounds the concurrent child count.
		parallel := step.MaxParallel
		if parallel <= 0 || !definition.batchByMaxParallel {
			parallel = len(indexes)
		}
		for offset := 0; offset < len(indexes); offset += parallel {
			end := min(offset+parallel, len(indexes))
			groups := make([][]int, 0, end-offset)
			if definition.batchByMaxParallel {
				for _, i := range indexes[offset:end] {
					groups = append(groups, []int{i})
				}
			} else {
				groups = append(groups, indexes)
			}
			futures := make([]workflow.ChildWorkflowFuture, len(groups))
			for groupIndex, group := range groups {
				batch := target
				batch.Identifiers = nil
				for _, i := range group {
					batch.Identifiers = append(batch.Identifiers, target.Identifiers[i])
				}
				contextTargets := make(map[devicetypes.ComponentType]common.Target, len(allTargets))
				for typ, value := range allTargets {
					contextTargets[typ] = value
				}
				contextTargets[target.Type] = batch
				childStep := step
				childStep.PreOperation, childStep.PostOperation = nil, nil
				childStep.MainOperation, childStep.DelayAfter = action, 0
				options := workflow.ChildWorkflowOptions{
					WorkflowID:               fmt.Sprintf("best-effort-%s-%d-%d-%d-%d", taskID, stage, stepIndex, actionIndex, group[0]),
					WorkflowExecutionTimeout: firmwareStepExecutionTimeout(childStep, batch.Len()),
				}
				futures[groupIndex] = workflow.ExecuteChildWorkflow(workflow.WithChildOptions(ctx, options), nameGenericComponentStepWorkflow, childStep, batch, info, contextTargets)
			}
			for groupIndex, future := range futures {
				err := future.Get(ctx, nil)
				if ctx.Err() != nil {
					return outcomes, ctx.Err()
				}
				if err == nil {
					continue
				}
				status := "failed"
				if bestEffortOutcomeUnknown(err) {
					status = "unknown"
				}
				for _, i := range groups[groupIndex] {
					outcomes[i].Status = status
					outcomes[i].Error = fmt.Sprintf("action %d (%s): %.512s", actionIndex, action.Name, err.Error())
					excluded[outcomes[i].Identifier] = fmt.Sprintf("preceding stage %d %s: %s", stage, status, outcomes[i].Error)
				}
				failures = append(failures, fmt.Errorf("%s %s: %w", action.Name, status, err))
			}
		}
	}
	if step.DelayAfter > 0 && ctx.Err() == nil {
		err := workflow.Sleep(ctx, step.DelayAfter)
		if err != nil {
			return outcomes, err
		}
	}
	return outcomes, errors.Join(failures...)
}

// bestEffortOutcomeUnknown follows all serialized causes, including an outer
// generic application error wrapping a typed observation timeout.
func bestEffortOutcomeUnknown(err error) bool {
	if temporal.IsTimeoutError(err) || temporal.IsCanceledError(err) || temporal.IsTerminatedError(err) {
		return true
	}
	for cause := err; cause != nil; cause = errors.Unwrap(cause) {
		applicationErr, ok := cause.(*temporal.ApplicationError)
		if ok && applicationErr.Type() == "UnknownOutcome" {
			return true
		}
	}
	return false
}
