// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

// APITargetResolution reports admission counts, separate from per-step execution outcomes.
type APITargetResolution struct {
	SkipUnaddressableTargets bool               `json:"skipUnaddressableTargets"`
	Selected                 int                `json:"selected"`
	Submitted                int                `json:"submitted"`
	Skipped                  []APISkippedTarget `json:"skipped"`
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
