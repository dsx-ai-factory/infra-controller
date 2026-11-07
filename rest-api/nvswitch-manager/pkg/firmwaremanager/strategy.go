// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"
)

// UpdateStrategy defines the interface for a firmware update strategy.
// Each strategy (Script, SSH, Redfish) implements this interface with its own
// sequence of steps and execution logic.
type UpdateStrategy interface {
	// Name returns the strategy type.
	Name() Strategy

	// Steps returns the ordered sequence of states for this strategy.
	// The executor will iterate through these states in order.
	// The update parameter allows strategies to return different steps based on
	// the component being updated and whether it's part of a bundle.
	Steps(update *FirmwareUpdate) []UpdateState

	// ExecuteStep performs the work for a single state in the update process.
	// Returns a StepOutcome indicating what the worker should do next:
	// - Wait: persist ExecContext and poll again after interval
	// - Transition: advance to the next state
	// - Failed: mark update as failed
	ExecuteStep(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome

	// GetCurrentVersion queries the current firmware version for a component.
	// This is used to populate VersionFrom before the update starts.
	GetCurrentVersion(ctx context.Context, tray *nvswitch.NVSwitchTray, component nvswitch.Component) (string, error)
}

// StrategyFactory creates an UpdateStrategy for a given strategy type.
type StrategyFactory func(config interface{}) UpdateStrategy
