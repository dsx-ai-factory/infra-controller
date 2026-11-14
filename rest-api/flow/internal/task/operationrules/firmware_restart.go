// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"slices"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/firmwareversion"
)

// RequiresNVOSComputeRestart reports whether a firmware request and its resolved
// rule update NVOS. Callers must also have selected an NVSwitch target. Admission
// and execution share this predicate so checks apply only to actual restart work.
func (rd *RuleDefinition) RequiresNVOSComputeRestart(info *operations.FirmwareControlTaskInfo) bool {
	if rd == nil || info == nil {
		return false
	}
	if len(info.SubTargets) != 0 && !slices.Contains(info.SubTargets, "nvos") {
		return false
	}
	_, selected := firmwareversion.Extract(info.TargetVersion, devicetypes.ComponentTypeNVSwitch)
	if !selected {
		return false
	}
	for _, step := range rd.Steps {
		if step.ComponentType != devicetypes.ComponentTypeNVSwitch {
			continue
		}
		for _, action := range step.OrderedActions() {
			if action.Name == ActionFirmwareControl {
				return true
			}
		}
	}
	return false
}
