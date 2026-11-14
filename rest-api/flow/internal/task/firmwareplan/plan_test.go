// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwareplan

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/stretchr/testify/require"
)

func TestBuild(t *testing.T) {
	compute, sw, ps := devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch, devicetypes.ComponentTypePowerShelf
	all := []devicetypes.ComponentType{compute, sw, ps}
	cases := []struct {
		name      string
		targets   []string
		version   string
		ruleTypes []devicetypes.ComponentType
		inventory []devicetypes.ComponentType
		want      map[devicetypes.ComponentType][]string
		wantError bool
	}{
		{name: "omitted selection preserves defaults"},
		{name: "empty selection preserves defaults", targets: []string{}},
		{name: "bios skips powershelf", targets: []string{"bios"}, want: map[devicetypes.ComponentType][]string{compute: {"bios"}, sw: {"bios"}}},
		{name: "nvos selects only switch", targets: []string{"nvos"}, want: map[devicetypes.ComponentType][]string{sw: {"nvos"}}},
		{name: "dpu selects only compute", targets: []string{"dpu"}, want: map[devicetypes.ComponentType][]string{compute: {"dpu"}}},
		{name: "mixed names partition per tray and normalize", targets: []string{" BMC ", "psu"}, want: map[devicetypes.ComponentType][]string{compute: {"bmc"}, sw: {"bmc"}, ps: {"psu"}}},
		{name: "overlapping names keep all applicable switch parts", targets: []string{"bmc", "nvos"}, want: map[devicetypes.ComponentType][]string{compute: {"bmc"}, sw: {"bmc", "nvos"}}},
		{name: "unknown name is not ignored beside a valid name", targets: []string{"bios", "typo"}, wantError: true},
		{name: "rule is a scope ceiling", targets: []string{"nvos"}, ruleTypes: []devicetypes.ComponentType{compute}, wantError: true},
		{name: "absent inventory is not a successful empty update", targets: []string{"nvos"}, inventory: []devicetypes.ComponentType{compute}, wantError: true},
		{name: "version scope cannot silently discard a requested target", targets: []string{"dpu", "nvos"}, version: `{"nvswitch":"switch-fw"}`, wantError: true},
		{name: "matching version scope narrows the plan", targets: []string{"bmc"}, version: `{"nvswitch":"switch-fw"}`, want: map[devicetypes.ComponentType][]string{sw: {"bmc"}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			types := tc.ruleTypes
			if types == nil {
				types = all
			}
			inventory := tc.inventory
			if inventory == nil {
				inventory = all
			}
			info := task.ExecutionInfo{RuleDefinition: &operationrules.RuleDefinition{}}
			for _, typ := range types {
				info.RuleDefinition.Steps = append(info.RuleDefinition.Steps, operationrules.SequenceStep{ComponentType: typ, MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl}})
			}
			for _, typ := range inventory {
				info.Components = append(info.Components, task.WorkflowComponent{ComponentID: "device", Type: typ})
			}
			input := operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: tc.targets, TargetVersion: tc.version, OverrideVersionCheck: true}
			plan, err := Build(info, input)
			if tc.wantError {
				require.Error(t, err)
				require.Nil(t, plan)
				return
			}
			require.NoError(t, err)
			var got map[devicetypes.ComponentType][]string
			if plan != nil {
				got = make(map[devicetypes.ComponentType][]string)
			}
			for typ, selected := range plan {
				got[typ] = selected.SubTargets
				require.Equal(t, input.TargetVersion, selected.TargetVersion)
				require.True(t, selected.OverrideVersionCheck)
				require.NotEmpty(t, selected.SubTargets)
			}
			require.Equal(t, tc.want, got)
			require.Equal(t, tc.targets, input.SubTargets)
		})
	}
}
