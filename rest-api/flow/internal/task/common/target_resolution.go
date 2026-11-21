// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// ExecutionMode selects the job's admission and failure-isolation behavior.
type ExecutionMode string

const (
	// ExecutionModeStrict preserves fail-fast execution.
	ExecutionModeStrict ExecutionMode = "Strict"
	// ExecutionModeBestEffort continues independent targets after a failure.
	ExecutionModeBestEffort ExecutionMode = "BestEffort"
)

// Valid accepts the omitted policy as Strict for predecessor requests and jobs.
func (p ExecutionMode) Valid() bool {
	return p == "" || p == ExecutionModeStrict || p == ExecutionModeBestEffort
}

// Effective returns the explicit policy, defaulting omission to Strict.
func (p ExecutionMode) Effective() ExecutionMode {
	if p == "" {
		return ExecutionModeStrict
	}
	return p
}

// TargetResolution is the immutable admission result stored with a task.
// Selected equals Submitted plus len(Skipped); execution outcomes are reported per rule step.
type TargetResolution struct {
	ExecutionMode ExecutionMode `json:"execution_mode"`
	// SkipUnaddressableTargets is read only for predecessor admission snapshots.
	// It does not enable BestEffort execution for already accepted jobs.
	SkipUnaddressableTargets bool            `json:"skip_unaddressable_targets,omitempty"`
	Selected                 int             `json:"selected"`
	Submitted                int             `json:"submitted"`
	Skipped                  []SkippedTarget `json:"skipped"`
}

// SkippedTarget identifies an expected component excluded before execution.
type SkippedTarget struct {
	ComponentID   uuid.UUID                 `json:"component_id"`
	ComponentType devicetypes.ComponentType `json:"component_type"`
	SlotID        int                       `json:"slot_id"`
	TrayIndex     int                       `json:"tray_index"`
	HostID        int                       `json:"host_id"`
	Reason        string                    `json:"reason"`
}
