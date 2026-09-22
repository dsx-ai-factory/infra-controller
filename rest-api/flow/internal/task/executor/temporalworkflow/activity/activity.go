// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/firmwareauth"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// Canonical Temporal activity names. These constants are the single source of
// truth: used in All() for worker registration and when scheduling via
// workflow.ExecuteActivity.
const (
	NameInjectExpectation         = "InjectExpectation"
	NamePowerControl              = "PowerControl"
	NameGetPowerStatus            = "GetPowerStatus"
	NameUpdateTaskStatus          = "UpdateTaskStatus"
	NameUpdateTaskReport          = "UpdateTaskReport"
	NameFirmwareControl           = "FirmwareControl"
	NameGetFirmwareStatus         = "GetFirmwareStatus"
	NameBringUpControl            = "BringUpControl"
	NameGetBringUpStatus          = "GetBringUpStatus"
	NameVerifyFirmwareConsistency = "VerifyFirmwareConsistency"
	NameDecommissionControl       = "DecommissionControl"
	NameGetDecommissionStatus     = "GetDecommissionStatus"
)

// InjectExpectation is a Temporal activity that registers expected component
// configurations with the appropriate component manager service.
func (a *Activities) InjectExpectation(
	ctx context.Context,
	target common.Target,
	info operations.InjectExpectationTaskInfo,
) error {
	injector, err := requireExpectationInjector(a.registry, target)
	if err != nil {
		return err
	}

	return injector.InjectExpectation(ctx, target, info)
}

// PowerControl is a Temporal activity that applies a power state transition
// to the target components via the appropriate component manager.
func (a *Activities) PowerControl(
	ctx context.Context,
	target common.Target,
	info operations.PowerControlTaskInfo,
) error {
	controller, err := requirePowerController(a.registry, target)
	if err != nil {
		return err
	}

	return controller.PowerControl(ctx, target, info)
}

// GetPowerStatus is a Temporal activity that queries current power states for
// all components in the target. Returns a map of component ID to PowerStatus.
func (a *Activities) GetPowerStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.PowerStatus, error) {
	reader, err := requirePowerStatusReader(a.registry, target)
	if err != nil {
		return nil, err
	}

	return reader.GetPowerStatus(ctx, target)
}

// UpdateTaskStatus is a Temporal activity that updates task status by ID.
func (a *Activities) UpdateTaskStatus(
	ctx context.Context,
	arg *task.TaskStatusUpdate,
) error {
	if a.updater == nil {
		return fmt.Errorf("task status updater is not configured")
	}

	if arg == nil || arg.ID == uuid.Nil {
		return fmt.Errorf("invalid task identifier")
	}

	return a.updater.UpdateTaskStatus(ctx, arg)
}

// UpdateTaskReport is a Temporal activity that merges a structured report
// snapshot without changing status or message.
func (a *Activities) UpdateTaskReport(
	ctx context.Context,
	arg *task.TaskReportUpdate,
) error {
	if a.reportUpdater == nil {
		return fmt.Errorf("task report updater is not configured")
	}

	if arg == nil || arg.ID == uuid.Nil {
		return fmt.Errorf("invalid task identifier")
	}

	return a.reportUpdater.UpdateTaskReport(ctx, arg)
}

// FirmwareControl initiates firmware update without waiting for completion.
// This activity returns immediately after the update request is accepted.
func (a *Activities) FirmwareControl(
	ctx context.Context,
	target common.Target,
	info operations.FirmwareControlTaskInfo,
) error {
	controller, err := requireFirmwareController(a.registry, target)
	if err != nil {
		return err
	}

	info.AccessToken, err = firmwareauth.DecryptFor(
		a.dataCipher,
		info.AuthenticationData,
		target.Type,
	)
	if err != nil {
		return err
	}

	return controller.FirmwareControl(ctx, target, info)
}

// GetFirmwareStatusResult is the result of GetFirmwareStatus activity.
type GetFirmwareStatusResult struct {
	// Statuses maps each component ID to its current firmware update state.
	Statuses map[string]operations.FirmwareUpdateStatus
}

// GetFirmwareStatus returns the current status of firmware updates.
// This activity is designed to be called repeatedly in a polling loop.
func (a *Activities) GetFirmwareStatus(
	ctx context.Context,
	target common.Target,
) (*GetFirmwareStatusResult, error) {
	reader, err := requireFirmwareStatusReader(a.registry, target)
	if err != nil {
		return nil, err
	}

	statuses, err := reader.GetFirmwareStatus(ctx, target)
	if err != nil {
		return nil, err
	}

	return &GetFirmwareStatusResult{Statuses: statuses}, nil
}

// BringUpControl opens the power-on gate for the target components. The info
// argument carries the parent BringUp task settings (e.g. override of the
// host-assignment safety gate) and is forwarded to the component manager.
func (a *Activities) BringUpControl(
	ctx context.Context,
	target common.Target,
	info operations.BringUpTaskInfo,
) error {
	controller, err := requireBringUpController(a.registry, target)
	if err != nil {
		return err
	}

	return controller.BringUpControl(ctx, target, info)
}

// GetBringUpStatusResult is the result of GetBringUpStatus activity.
type GetBringUpStatusResult struct {
	// States maps each component ID to its current bring-up state.
	States map[string]operations.MachineBringUpState
}

// GetBringUpStatus returns the bring-up state for target components.
func (a *Activities) GetBringUpStatus(
	ctx context.Context,
	target common.Target,
) (*GetBringUpStatusResult, error) {
	reader, err := requireBringUpStatusReader(a.registry, target)
	if err != nil {
		return nil, err
	}

	states, err := reader.GetBringUpStatus(ctx, target)
	if err != nil {
		return nil, err
	}

	return &GetBringUpStatusResult{States: states}, nil
}

// DecommissionControl initiates decommission of the target components via
// the appropriate component manager. Returns immediately after the request
// is accepted; callers should poll GetDecommissionStatus for completion.
func (a *Activities) DecommissionControl(
	ctx context.Context,
	target common.Target,
	info operations.DecommissionTaskInfo,
) error {
	decommissioner, err := requireDecommissioner(a.registry, target)
	if err != nil {
		return err
	}

	return decommissioner.Decommission(ctx, target, info)
}

// GetDecommissionStatusResult is the result of the GetDecommissionStatus activity.
type GetDecommissionStatusResult struct {
	// States maps each found component ID to its current raw decommission state
	// string as returned by the component manager (e.g. "Decommissioning/...",
	// "Decommissioned", "Failed/..."). Only IDs that Core returned a record for
	// are present here.
	States map[string]string
	// NotFound holds component IDs for which the reader could not return a
	// usable state. This can mean the record is absent or that its state was
	// omitted; it is not a terminal-success signal.
	NotFound []string
}

// GetDecommissionStatus returns the decommission state for target components.
// This activity is designed to be called repeatedly in a polling loop.
// Component IDs with an empty state are returned in NotFound rather than as
// empty strings in States, so the caller can reject the ambiguous result.
func (a *Activities) GetDecommissionStatus(
	ctx context.Context,
	target common.Target,
) (*GetDecommissionStatusResult, error) {
	reader, err := requireDecommissionStatusReader(a.registry, target)
	if err != nil {
		return nil, err
	}

	rawStates, err := reader.GetDecommissionStatus(ctx, target)
	if err != nil {
		return nil, err
	}

	result := &GetDecommissionStatusResult{
		States: make(map[string]string, len(rawStates)),
	}
	for id, state := range rawStates {
		if state == "" {
			result.NotFound = append(result.NotFound, id)
		} else {
			result.States[id] = state
		}
	}
	return result, nil
}

// VerifyFirmwareConsistency checks that all target components report the
// same firmware version set. Only supported by component managers that
// implement FirmwareConsistencyChecker.
func (a *Activities) VerifyFirmwareConsistency(
	ctx context.Context,
	target common.Target,
) error {
	checker, err := requireFirmwareConsistencyChecker(a.registry, target)
	if err != nil {
		return err
	}

	return checker.VerifyFirmwareConsistency(ctx, target)
}
