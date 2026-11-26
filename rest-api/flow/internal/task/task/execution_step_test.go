// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"testing"

	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExecutionStep_ResolveOperationInfo(t *testing.T) {
	for _, tc := range []struct {
		name         string
		raw          string
		wantOverride bool
		wantError    string
	}{
		{name: "inherit original task options", wantOverride: true},
		{name: "explicit step options", raw: `{"operation":1}`, wantOverride: false},
		{name: "malformed options", raw: `{`, wantError: "failed to unmarshal"},
		{name: "unsupported operation", raw: `{"operation":0}`, wantError: "operation"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			step := ExecutionStep{OperationInfo: []byte(tc.raw)}
			op, err := step.ResolveOperationInfo(ExecutionInfo{OperationType: taskcommon.TaskTypeFirmwareControl,
				OperationInfo: []byte(`{"operation":1,"override_readiness_check":true}`)})
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.wantOverride, op.(*operations.FirmwareControlTaskInfo).OverrideReadinessCheck)
		})
	}
}

func TestExecutionInfo_ValidateAdditionalSteps(t *testing.T) {
	for _, tc := range []struct {
		name      string
		change    func(*ExecutionInfo)
		wantError string
	}{
		{name: "bound step follows sparse main stage"},
		{name: "parallel additional types", change: func(info *ExecutionInfo) {
			other := info.AdditionalSteps[0]
			other.Definition.ComponentType = devicetypes.ComponentTypeNVSwitch
			other.Components = []WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "switch"}}
			info.AdditionalSteps = append(info.AdditionalSteps, other)
		}},
		{name: "overlapping main stage", change: func(info *ExecutionInfo) { info.AdditionalSteps[0].Definition.Stage = 8 }, wantError: "ordered after main"},
		{name: "unordered stages", change: func(info *ExecutionInfo) {
			later := info.AdditionalSteps[0]
			later.Definition.Stage = 10
			info.AdditionalSteps = append([]ExecutionStep{later}, info.AdditionalSteps...)
		}, wantError: "ordered after main"},
		{name: "duplicate type and stage", change: func(info *ExecutionInfo) {
			info.AdditionalSteps = append(info.AdditionalSteps, info.AdditionalSteps[0])
		}, wantError: "duplicates component type"},
		{name: "missing targets", change: func(info *ExecutionInfo) { info.AdditionalSteps[0].Components = nil }, wantError: "no targets"},
		{name: "wrong target type", change: func(info *ExecutionInfo) {
			info.AdditionalSteps[0].Components[0].Type = devicetypes.ComponentTypeNVSwitch
		}, wantError: "invalid target"},
		{name: "unaddressable target", change: func(info *ExecutionInfo) { info.AdditionalSteps[0].Components[0].ComponentID = "" }, wantError: "invalid target"},
		{name: "mixed identifier batch needs every MAC", change: func(info *ExecutionInfo) {
			info.AdditionalSteps[0].Components = append(info.AdditionalSteps[0].Components,
				WorkflowComponent{Type: devicetypes.ComponentTypeCompute, MACAddress: "aa:bb:cc:dd:ee:01"})
		}, wantError: "requires a MAC address for every target"},
		{name: "mixed identifier batch with all MACs", change: func(info *ExecutionInfo) {
			info.AdditionalSteps[0].Components[0].MACAddress = "aa:bb:cc:dd:ee:02"
			info.AdditionalSteps[0].Components = append(info.AdditionalSteps[0].Components,
				WorkflowComponent{Type: devicetypes.ComponentTypeCompute, MACAddress: "aa:bb:cc:dd:ee:01"})
		}},
		{name: "firmware version excludes bound type", change: func(info *ExecutionInfo) {
			info.OperationInfo = []byte(`{"operation":1,"target_version":"{\"nvswitch\":\"switch-fw\"}"}`)
			info.AdditionalSteps[0].Definition.MainOperation = operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl}
		}, wantError: "firmware version excludes"},
		{name: "explicit version override selects bound type", change: func(info *ExecutionInfo) {
			info.OperationInfo = []byte(`{"operation":1,"target_version":"{\"nvswitch\":\"switch-fw\"}"}`)
			info.AdditionalSteps[0].Definition.MainOperation = operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl}
			info.AdditionalSteps[0].OperationInfo = []byte(`{"operation":1,"target_version":"compute-fw"}`)
		}},
		{name: "firmware version selection does not suppress power steps", change: func(info *ExecutionInfo) {
			info.OperationInfo = []byte(`{"operation":1,"target_version":"{\"nvswitch\":\"switch-fw\"}"}`)
			info.AdditionalSteps[0].Definition.MainOperation = operationrules.ActionConfig{Name: operationrules.ActionPowerControl,
				Parameters: map[string]any{operationrules.ParamOperation: "power_on"}}
		}},
		{name: "missing rule", change: func(info *ExecutionInfo) { info.RuleDefinition = nil }, wantError: "require a main rule"},
		{name: "fixed workflow rejects extra steps", change: func(info *ExecutionInfo) { info.OperationType = taskcommon.TaskTypeComponentReset }, wantError: "does not execute"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			def := operationrules.SequenceStep{ComponentType: devicetypes.ComponentTypeCompute, Stage: 9,
				MainOperation: operationrules.ActionConfig{Name: operationrules.ActionSleep,
					Parameters: map[string]any{operationrules.ParamDuration: "1s"}}}
			info := ExecutionInfo{OperationType: taskcommon.TaskTypeFirmwareControl, OperationInfo: []byte(`{"operation":1}`),
				RuleDefinition:  &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{Stage: 8}}},
				AdditionalSteps: []ExecutionStep{{Definition: def, Components: []WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute"}}}}}
			if tc.change != nil {
				tc.change(&info)
			}
			err := info.ValidateAdditionalSteps()
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
			} else {
				require.NoError(t, err)
			}
		})
	}
}
