// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ExtractPowerState derives an operations.PowerStatus from the first
// ComputerSystem in a site exploration report.  Returns PowerStatusUnknown
// when the report is nil or contains no systems.
func ExtractPowerState(report *corev1.EndpointExplorationReport) operations.PowerStatus {
	if report == nil {
		return operations.PowerStatusUnknown
	}
	systems := report.GetSystems()
	if len(systems) == 0 {
		return operations.PowerStatusUnknown
	}
	switch systems[0].GetPowerState() {
	case corev1.ComputerSystemPowerState_On:
		return operations.PowerStatusOn
	case corev1.ComputerSystemPowerState_Off:
		return operations.PowerStatusOff
	default:
		return operations.PowerStatusUnknown
	}
}
