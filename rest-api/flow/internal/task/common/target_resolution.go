// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TargetResolutionMode controls admission when selected components cannot be addressed.
type TargetResolutionMode string

const (
	// TargetResolutionStrict requires every selected component to be addressable.
	TargetResolutionStrict TargetResolutionMode = "Strict"
	// TargetResolutionBestEffort records unaddressable components as skipped.
	TargetResolutionBestEffort TargetResolutionMode = "BestEffort"
)

// TargetResolution is the immutable admission result stored with a task.
// Selected equals Submitted plus len(Skipped); execution outcomes are reported per rule step.
type TargetResolution struct {
	Mode      TargetResolutionMode `json:"mode"`
	Selected  int                  `json:"selected"`
	Submitted int                  `json:"submitted"`
	Skipped   []SkippedTarget      `json:"skipped"`
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
