// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// TargetResolutionMode selects addressability policy independently of readiness.
type TargetResolutionMode string

const (
	// TargetResolutionStrict requires all selected components to be addressable.
	TargetResolutionStrict TargetResolutionMode = "Strict"
	// TargetResolutionBestEffort skips unaddressable components with a report entry.
	TargetResolutionBestEffort TargetResolutionMode = "BestEffort"
)

// Validate accepts omission as Strict and rejects unsupported modes.
func (m TargetResolutionMode) Validate() error {
	switch m {
	case "", TargetResolutionStrict, TargetResolutionBestEffort:
		return nil
	default:
		return fmt.Errorf("must be Strict or BestEffort")
	}
}

// ToProto maps a validated request mode to the Flow wire enum.
func (m TargetResolutionMode) ToProto() flowv1.TargetResolutionMode {
	if m == "" {
		return flowv1.TargetResolutionMode_TARGET_RESOLUTION_MODE_UNSPECIFIED
	}
	if m == TargetResolutionBestEffort {
		return flowv1.TargetResolutionMode_TARGET_RESOLUTION_MODE_BEST_EFFORT
	}
	return flowv1.TargetResolutionMode_TARGET_RESOLUTION_MODE_STRICT
}

// APITargetResolution reports admission counts, separate from per-step execution outcomes.
type APITargetResolution struct {
	Mode      TargetResolutionMode `json:"mode"`
	Selected  int                  `json:"selected"`
	Submitted int                  `json:"submitted"`
	Skipped   []APISkippedTarget   `json:"skipped"`
}

// APISkippedTarget identifies one expected component excluded at admission.
type APISkippedTarget struct {
	ComponentID   string `json:"componentId"`
	ComponentType string `json:"componentType"`
	SlotID        int    `json:"slotId"`
	TrayIndex     int    `json:"trayIndex"`
	HostID        int    `json:"hostId"`
	Reason        string `json:"reason"`
}
