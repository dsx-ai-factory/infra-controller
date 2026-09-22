// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/stretchr/testify/require"
)

func TestOperation_Clone(t *testing.T) {
	tests := map[string]Operation{
		"power control": &PowerControlTaskInfo{
			Operation:              PowerOperationForcePowerOff,
			Forced:                 true,
			RuleID:                 "power-rule",
			OverrideReadinessCheck: true,
		},
		"inject expectation": &InjectExpectationTaskInfo{
			Info: json.RawMessage(`{"expected":"value"}`),
		},
		"bring up": &BringUpTaskInfo{
			RuleID:                 "bring-up-rule",
			OpCode:                 "ingest",
			OverrideReadinessCheck: true,
		},
		"firmware control": &FirmwareControlTaskInfo{
			Operation:              FirmwareOperationUpgrade,
			TargetVersion:          "1.2.3",
			StartTime:              123,
			EndTime:                456,
			RuleID:                 "firmware-rule",
			SubTargets:             []string{"bmc", "bios"},
			OverrideReadinessCheck: true,
		},
		"decommission": &DecommissionTaskInfo{RuleID: "decommission-rule"},
	}

	for name, operation := range tests {
		t.Run(name, func(t *testing.T) {
			cloned := operation.Clone()
			require.Equal(t, operation, cloned)
			require.NotSame(t, operation, cloned)
		})
	}
}

func TestOperation_Validate(t *testing.T) {
	tests := map[string]struct {
		operation Operation
		wantErr   string
	}{
		"power control": {
			operation: &PowerControlTaskInfo{Operation: PowerOperationForcePowerOff},
		},
		"unknown power control": {
			operation: &PowerControlTaskInfo{},
			wantErr:   "invalid power control operation",
		},
		"out-of-range power control": {
			operation: &PowerControlTaskInfo{Operation: PowerOperation(100)},
			wantErr:   "invalid power control operation",
		},
		"inject expectation": {operation: &InjectExpectationTaskInfo{}},
		"bring up default":   {operation: &BringUpTaskInfo{}},
		"bring up ingest": {
			operation: &BringUpTaskInfo{OpCode: taskcommon.OpCodeIngest},
		},
		"invalid bring up code": {
			operation: &BringUpTaskInfo{OpCode: "full"},
			wantErr:   `operation code "full" is invalid for task type "bring_up"`,
		},
		"firmware control": {
			operation: &FirmwareControlTaskInfo{Operation: FirmwareOperationUpgrade},
		},
		"unknown firmware control": {
			operation: &FirmwareControlTaskInfo{},
			wantErr:   "invalid firmware control operation",
		},
		"unsupported firmware version operation": {
			operation: &FirmwareControlTaskInfo{Operation: FirmwareOperationVersion},
			wantErr:   "invalid firmware control operation",
		},
		"out-of-range firmware control": {
			operation: &FirmwareControlTaskInfo{Operation: FirmwareOperation(100)},
			wantErr:   "invalid firmware control operation",
		},
		"decommission": {operation: &DecommissionTaskInfo{}},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.operation.Validate()
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}
			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestOperation_ValidateRejectsNil(t *testing.T) {
	tests := map[string]Operation{
		"power control":      (*PowerControlTaskInfo)(nil),
		"inject expectation": (*InjectExpectationTaskInfo)(nil),
		"bring up":           (*BringUpTaskInfo)(nil),
		"firmware control":   (*FirmwareControlTaskInfo)(nil),
		"decommission":       (*DecommissionTaskInfo)(nil),
	}

	for name, operation := range tests {
		t.Run(name, func(t *testing.T) {
			require.Error(t, operation.Validate())
		})
	}
}

func TestOperation_CloneMutablePayload(t *testing.T) {
	tests := map[string]struct {
		operation Operation
		mutate    func(Operation)
	}{
		"inject expectation": {
			operation: &InjectExpectationTaskInfo{
				Info: json.RawMessage(`{"expected":"value"}`),
			},
			mutate: func(operation Operation) {
				operation.(*InjectExpectationTaskInfo).Info[0] = '['
			},
		},
		"firmware subtargets": {
			operation: &FirmwareControlTaskInfo{
				Operation:  FirmwareOperationUpgrade,
				SubTargets: []string{"bmc"},
			},
			mutate: func(operation Operation) {
				operation.(*FirmwareControlTaskInfo).SubTargets[0] = "bios"
			},
		},
		"firmware authentication data": {
			operation: &FirmwareControlTaskInfo{
				Operation: FirmwareOperationUpgrade,
				AuthenticationData: &secret.EncryptedData{
					Version: 1, KeyID: "key", Ciphertext: []byte("ciphertext"),
				},
			},
			mutate: func(operation Operation) {
				firmware := operation.(*FirmwareControlTaskInfo)
				firmware.AuthenticationData.KeyID = "changed"
				firmware.AuthenticationData.Ciphertext[0] = 'x'
			},
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			cloned := test.operation.Clone()
			test.mutate(cloned)
			require.NotEqual(t, test.operation, cloned)
		})
	}
}

func TestOperation_CloneNilReceiver(t *testing.T) {
	tests := map[string]Operation{
		"power control":      (*PowerControlTaskInfo)(nil),
		"inject expectation": (*InjectExpectationTaskInfo)(nil),
		"bring up":           (*BringUpTaskInfo)(nil),
		"firmware control":   (*FirmwareControlTaskInfo)(nil),
		"decommission":       (*DecommissionTaskInfo)(nil),
	}

	for name, operation := range tests {
		t.Run(name, func(t *testing.T) {
			require.Nil(t, operation.Clone())
		})
	}
}
