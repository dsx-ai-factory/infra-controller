// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestIsValidOperation(t *testing.T) {
	tests := []struct {
		name      string
		opType    common.TaskType
		operation string
		want      bool
	}{
		// Valid power control operations
		{
			name:      "valid power_on",
			opType:    common.TaskTypePowerControl,
			operation: SequencePowerOn,
			want:      true,
		},
		{
			name:      "valid force_power_on",
			opType:    common.TaskTypePowerControl,
			operation: SequenceForcePowerOn,
			want:      true,
		},
		{
			name:      "valid power_off",
			opType:    common.TaskTypePowerControl,
			operation: SequencePowerOff,
			want:      true,
		},
		{
			name:      "valid force_power_off",
			opType:    common.TaskTypePowerControl,
			operation: SequenceForcePowerOff,
			want:      true,
		},
		{
			name:      "valid restart",
			opType:    common.TaskTypePowerControl,
			operation: SequenceRestart,
			want:      true,
		},
		{
			name:      "valid force_restart",
			opType:    common.TaskTypePowerControl,
			operation: SequenceForceRestart,
			want:      true,
		},
		{
			name:      "valid warm_reset",
			opType:    common.TaskTypePowerControl,
			operation: SequenceWarmReset,
			want:      true,
		},
		{
			name:      "valid cold_reset",
			opType:    common.TaskTypePowerControl,
			operation: SequenceColdReset,
			want:      true,
		},
		// Valid firmware control operations
		{
			name:      "valid upgrade",
			opType:    common.TaskTypeFirmwareControl,
			operation: SequenceUpgrade,
			want:      true,
		},
		{
			name:      "valid downgrade",
			opType:    common.TaskTypeFirmwareControl,
			operation: SequenceDowngrade,
			want:      true,
		},
		{
			name:      "valid rollback",
			opType:    common.TaskTypeFirmwareControl,
			operation: SequenceRollback,
			want:      true,
		},
		// Invalid operations
		{
			name:      "invalid operation for power control",
			opType:    common.TaskTypePowerControl,
			operation: "invalid_operation",
			want:      false,
		},
		{
			name:      "invalid operation for firmware control",
			opType:    common.TaskTypeFirmwareControl,
			operation: "invalid_operation",
			want:      false,
		},
		{
			name:      "empty operation",
			opType:    common.TaskTypePowerControl,
			operation: "",
			want:      false,
		},
		{
			name:      "power operation for firmware type",
			opType:    common.TaskTypeFirmwareControl,
			operation: SequencePowerOn,
			want:      false,
		},
		{
			name:      "firmware operation for power type",
			opType:    common.TaskTypePowerControl,
			operation: SequenceUpgrade,
			want:      false,
		},
		// Unknown operation type
		{
			name:      "unknown operation type",
			opType:    common.TaskTypeUnknown,
			operation: SequencePowerOn,
			want:      false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := IsValidOperation(tt.opType, tt.operation)
			if got != tt.want {
				t.Errorf("IsValidOperation(%v, %q) = %v, want %v",
					tt.opType, tt.operation, got, tt.want)
			}
		})
	}
}

func TestSequenceConstantsMatchSharedCodes(t *testing.T) {
	tests := []struct {
		name          string
		sequenceConst string
		sharedCode    string
	}{
		{"PowerOn", SequencePowerOn, common.OpCodePowerControlPowerOn},
		{"ForcePowerOn", SequenceForcePowerOn, common.OpCodePowerControlForcePowerOn},
		{"PowerOff", SequencePowerOff, common.OpCodePowerControlPowerOff},
		{"ForcePowerOff", SequenceForcePowerOff, common.OpCodePowerControlForcePowerOff},
		{"Restart", SequenceRestart, common.OpCodePowerControlRestart},
		{"ForceRestart", SequenceForceRestart, common.OpCodePowerControlForceRestart},
		{"WarmReset", SequenceWarmReset, common.OpCodePowerControlWarmReset},
		{"ColdReset", SequenceColdReset, common.OpCodePowerControlColdReset},
		{"Upgrade", SequenceUpgrade, common.OpCodeFirmwareControlUpgrade},
		{"Downgrade", SequenceDowngrade, common.OpCodeFirmwareControlDowngrade},
		{"Rollback", SequenceRollback, common.OpCodeFirmwareControlRollback},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.sequenceConst != tt.sharedCode {
				t.Errorf("%s: sequence constant %q != shared code %q",
					tt.name, tt.sequenceConst, tt.sharedCode)
			}
		})
	}
}
