// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package firmwareplan resolves explicit firmware sub-parts against the retained
// rule, inventory membership and per-tray version selection without side effects.
package firmwareplan

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/firmwarecomponents"
)

// Build returns per-tray firmware options for explicit sub-targets. Every name
// must match a firmware action in the selected rule and retained inventory.
// Missing types must skip all their rule steps. Nil means no explicit selection:
// callers must retain the existing default behavior rather than filter types.
func Build(info task.ExecutionInfo, firmware operations.FirmwareControlTaskInfo) (map[devicetypes.ComponentType]operations.FirmwareControlTaskInfo, error) {
	if len(firmware.SubTargets) == 0 {
		return nil, nil
	}
	present := make(map[devicetypes.ComponentType]bool)
	for _, component := range info.Components {
		present[component.Type] = true
	}
	plan := make(map[devicetypes.ComponentType]operations.FirmwareControlTaskInfo)
	matched := make(map[string]bool)
	if info.RuleDefinition != nil {
		for _, step := range info.RuleDefinition.Steps {
			if !present[step.ComponentType] {
				continue
			}
			_, selectedType := TargetVersion(firmware.TargetVersion, step.ComponentType)
			if !selectedType {
				continue
			}
			for _, action := range step.OrderedActions() {
				if action.Name != operationrules.ActionFirmwareControl {
					continue
				}
				subset := firmwarecomponents.ForType(firmware.SubTargets, step.ComponentType)
				if len(subset) == 0 {
					continue
				}
				selected := firmware
				selected.SubTargets = subset
				plan[step.ComponentType] = selected
				for _, name := range subset {
					matched[name] = true
				}
			}
		}
	}
	for _, name := range firmware.SubTargets {
		if !matched[strings.ToLower(strings.TrimSpace(name))] {
			return nil, fmt.Errorf("firmware target %q has no matching firmware action in the selected rule, inventory and version scope", name)
		}
	}
	return plan, nil
}

// TargetVersion resolves one tray's version input. A recognized per-tray JSON
// mapping excludes missing types; shared input passes through unchanged. String
// mapping values are unquoted and object values remain raw JSON.
func TargetVersion(raw string, typ devicetypes.ComponentType) (string, bool) {
	if raw == "" {
		return "", true
	}
	var layered map[string]json.RawMessage
	err := json.Unmarshal([]byte(raw), &layered)
	if err != nil {
		return raw, true
	}
	key := strings.ToLower(devicetypes.ComponentTypeToString(typ))
	section, exists := layered[key]
	if exists {
		if len(section) > 0 && section[0] == '"' {
			var value string
			err := json.Unmarshal(section, &value)
			if err == nil {
				return value, true
			}
		}
		return string(section), true
	}
	for _, known := range []string{"compute", "nvswitch", "powershelf"} {
		_, found := layered[known]
		if found {
			return "", false
		}
	}
	return raw, true
}
