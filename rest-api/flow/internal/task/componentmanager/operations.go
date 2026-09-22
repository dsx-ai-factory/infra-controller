// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// Operation interfaces describe the optional callable behaviors behind
// component manager capabilities. A manager should implement the interface that
// matches each capability declared by its descriptor.

// ExpectationInjector is implemented by component managers that support
// registering expected component configuration.
//
// Required descriptor capability: capability.CapabilityInjectExpectation.
type ExpectationInjector interface {
	// InjectExpectation registers expected component configurations with the
	// component manager service for the target components.
	InjectExpectation(ctx context.Context, target common.Target, info operations.InjectExpectationTaskInfo) error //nolint
}

// PowerController is implemented by component managers that support power
// state transitions.
//
// Required descriptor capability: capability.CapabilityPowerControl.
type PowerController interface {
	// PowerControl applies a power state transition to the target components.
	PowerControl(ctx context.Context, target common.Target, info operations.PowerControlTaskInfo) error //nolint
}

// PowerStatusReader is implemented by component managers that can report power
// state.
//
// Required descriptor capability: capability.CapabilityPowerStatus.
type PowerStatusReader interface {
	// GetPowerStatus queries the current power state of each component in the
	// target. Keys are the requested Core IDs or MAC addresses. Missing or failed
	// responses are omitted; an Unknown value means a response was received but
	// its power state could not be determined.
	GetPowerStatus(ctx context.Context, target common.Target) (map[string]operations.PowerStatus, error) //nolint
}

// FirmwareController is implemented by component managers that support
// initiating firmware operations.
//
// Required descriptor capability: capability.CapabilityFirmwareControl.
type FirmwareController interface {
	// FirmwareControl initiates a firmware update without waiting for completion.
	// Returns immediately after the update request is accepted.
	FirmwareControl(ctx context.Context, target common.Target, info operations.FirmwareControlTaskInfo) error //nolint
}

// FirmwareStatusReader is implemented by component managers that can report
// firmware operation state.
//
// Required descriptor capability: capability.CapabilityFirmwareStatus.
type FirmwareStatusReader interface {
	// GetFirmwareStatus returns the current firmware update state for each
	// component in the target. Returns a map of component ID to FirmwareUpdateStatus.
	GetFirmwareStatus(ctx context.Context, target common.Target) (map[string]operations.FirmwareUpdateStatus, error) //nolint
}

// BringUpController is implemented by component managers that support bring-up
// control operations.
//
// Required descriptor capability: capability.CapabilityBringUpControl.
type BringUpController interface {
	// BringUpControl opens the power-on gate for the target components, allowing
	// them to proceed through the bring-up sequence. The info argument carries
	// the parent BringUp task settings — notably OverrideReadinessCheck —
	// because bring-up can power-cycle hosts and must consult the same readiness
	// gate as PowerControl / FirmwareControl.
	BringUpControl(ctx context.Context, target common.Target, info operations.BringUpTaskInfo) error //nolint
}

// BringUpStatusReader is implemented by component managers that can report
// bring-up state.
//
// Required descriptor capability: capability.CapabilityBringUpStatus.
type BringUpStatusReader interface {
	// GetBringUpStatus returns the current bring-up state for each component in
	// the target. Returns a map of component ID to MachineBringUpState.
	GetBringUpStatus(ctx context.Context, target common.Target) (map[string]operations.MachineBringUpState, error)
}

// Decommissioner is implemented by component managers that support
// decommissioning their components.
//
// Required descriptor capability: capability.CapabilityDecommissionControl.
type Decommissioner interface {
	// Decommission initiates the decommission of the target components. It
	// returns immediately after the request is accepted; callers should poll
	// GetDecommissionStatus for completion.
	Decommission(ctx context.Context, target common.Target, info operations.DecommissionTaskInfo) error //nolint
}

// DecommissionStatusReader is implemented by component managers that can
// report decommission progress.
//
// Required descriptor capability: capability.CapabilityDecommissionStatus.
type DecommissionStatusReader interface {
	// GetDecommissionStatus returns the current decommission state for each
	// component in the target. Returns a map of component ID to raw state
	// string (e.g. "Decommissioning/...", "Decommissioned", "Failed/...").
	GetDecommissionStatus(ctx context.Context, target common.Target) (map[string]string, error) //nolint
}

// FirmwareConsistencyChecker is an optional interface for component managers
// that can verify firmware version consistency across a set of components.
//
// Required descriptor capability: capability.CapabilityFirmwareConsistencyCheck.
type FirmwareConsistencyChecker interface {
	// VerifyFirmwareConsistency checks that all target components report the same
	// firmware version set. Returns an error if versions diverge.
	VerifyFirmwareConsistency(ctx context.Context, target common.Target) error
}
