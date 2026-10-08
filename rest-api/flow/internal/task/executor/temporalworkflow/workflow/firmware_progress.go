// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const (
	firmwareReportCountersChangeID = "firmware-report-component-counters"
	firmwareStepProgressSignalName = "firmware-step-progress"
)

type firmwareProgressContextKey struct{}

type firmwareStepProgress struct {
	StageNumber         int
	ComponentType       string
	CompletedComponents int
	FailedComponents    int
}

type firmwareProgressAccumulator struct {
	stageNumber   int
	componentType string
	terminalState map[string]operations.FirmwareUpdateState
}

func (a *firmwareProgressAccumulator) update(
	target common.Target,
	statuses map[string]operations.FirmwareUpdateStatus,
) firmwareStepProgress {
	for _, componentID := range target.Identifiers {
		delete(a.terminalState, componentID)
		status, present := statuses[componentID]
		if present && status.State.IsTerminal() {
			a.terminalState[componentID] = status.State
		}
	}

	completedComponents := 0
	failedComponents := 0
	for _, state := range a.terminalState {
		switch state {
		case operations.FirmwareUpdateStateCompleted:
			completedComponents++
		case operations.FirmwareUpdateStateFailed:
			failedComponents++
		}
	}

	return firmwareStepProgress{
		StageNumber:         a.stageNumber,
		ComponentType:       a.componentType,
		CompletedComponents: completedComponents,
		FailedComponents:    failedComponents,
	}
}

type firmwareProgressReporter struct {
	parentWorkflowID string
	parentRunID      string
	accumulator      firmwareProgressAccumulator
}

func withFirmwareProgressReporter(
	ctx workflow.Context,
	step operationrules.SequenceStep,
	target common.Target,
) workflow.Context {
	if !stepReportsFirmwareProgress(step) || !firmwareReportCountersEnabled(ctx) {
		return ctx
	}

	parent := workflow.GetInfo(ctx).ParentWorkflowExecution
	if parent == nil {
		return ctx
	}

	reporter := &firmwareProgressReporter{
		parentWorkflowID: parent.ID,
		parentRunID:      parent.RunID,
		accumulator: firmwareProgressAccumulator{
			stageNumber:   step.Stage,
			componentType: devicetypes.ComponentTypeToString(step.ComponentType),
			terminalState: make(map[string]operations.FirmwareUpdateState, target.Len()),
		},
	}
	return workflow.WithValue(ctx, firmwareProgressContextKey{}, reporter)
}

func reportFirmwareProgress(
	ctx workflow.Context,
	target common.Target,
	statuses map[string]operations.FirmwareUpdateStatus,
) {
	reporter, ok := ctx.Value(firmwareProgressContextKey{}).(*firmwareProgressReporter)
	if !ok {
		return
	}

	progress := reporter.accumulator.update(target, statuses)
	err := workflow.SignalExternalWorkflow(
		ctx,
		reporter.parentWorkflowID,
		reporter.parentRunID,
		firmwareStepProgressSignalName,
		progress,
	).Get(ctx, nil)
	if err != nil {
		log.Warn().
			Err(err).
			Int("stage_number", progress.StageNumber).
			Str("component_type", progress.ComponentType).
			Msg("Failed to report firmware component progress")
	}
}

func firmwareReportCountersEnabled(ctx workflow.Context) bool {
	version := workflow.GetVersion(
		ctx,
		firmwareReportCountersChangeID,
		workflow.DefaultVersion,
		workflow.Version(1),
	)
	return version != workflow.DefaultVersion
}

func stepReportsFirmwareProgress(step operationrules.SequenceStep) bool {
	for _, action := range step.OrderedActions() {
		if action.Name == operationrules.ActionFirmwareControl {
			return true
		}
	}
	return false
}

func firmwareProgressReportingEnabled(
	ctx workflow.Context,
	steps []operationrules.SequenceStep,
) bool {
	for _, step := range steps {
		if stepReportsFirmwareProgress(step) {
			return firmwareReportCountersEnabled(ctx)
		}
	}
	return false
}

func waitForChildrenAndFirmwareProgress(
	ctx workflow.Context,
	taskID uuid.UUID,
	tracker *report.Tracker,
	futures []childWorkflowEntry,
) error {
	progressChannel := workflow.GetSignalChannel(ctx, firmwareStepProgressSignalName)
	selector := workflow.NewSelector(ctx)
	remaining := len(futures)
	var childErr error

	for _, entry := range futures {
		entry := entry
		selector.AddFuture(entry.future, func(future workflow.Future) {
			remaining--
			err := future.Get(ctx, nil)
			tracker.FinishStep(entry.stageNumber, devicetypes.ComponentTypeToString(entry.componentType), err, workflow.Now(ctx))
			if err != nil && childErr == nil {
				childErr = fmt.Errorf(
					"component type %s failed: %w",
					devicetypes.ComponentTypeToString(entry.componentType),
					err,
				)
				return
			}
			if err == nil {
				log.Info().
					Str("component_type", devicetypes.ComponentTypeToString(entry.componentType)).
					Msg("Component step completed successfully")
			}
		})
	}

	selector.AddReceive(progressChannel, func(channel workflow.ReceiveChannel, _ bool) {
		var progress firmwareStepProgress
		channel.Receive(ctx, &progress)
		applyFirmwareStepProgress(ctx, taskID, tracker, progress)
	})

	// A failed child must not prevent other children from reporting their final progress.
	for remaining > 0 {
		selector.Select(ctx)
	}
	drainFirmwareStepProgress(ctx, taskID, tracker, progressChannel)

	return childErr
}

func drainFirmwareStepProgress(
	ctx workflow.Context,
	taskID uuid.UUID,
	tracker *report.Tracker,
	progressChannel workflow.ReceiveChannel,
) {
	var progress firmwareStepProgress
	for progressChannel.ReceiveAsync(&progress) {
		applyFirmwareStepProgress(ctx, taskID, tracker, progress)
	}
}

func applyFirmwareStepProgress(
	ctx workflow.Context,
	taskID uuid.UUID,
	tracker *report.Tracker,
	progress firmwareStepProgress,
) {
	err := tracker.SetStepProgress(
		progress.StageNumber,
		progress.ComponentType,
		progress.CompletedComponents,
		progress.FailedComponents,
	)
	if err != nil {
		log.Warn().Err(err).Msg("Ignoring invalid firmware component progress")
		return
	}
	updateTaskReportBestEffort(ctx, taskID, tracker.Report)
}
