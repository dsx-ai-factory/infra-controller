// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestPowerOperationCodeString(t *testing.T) {
	tests := []struct {
		name         string
		operation    PowerOperation
		expectedCode string
	}{
		{"PowerOn", PowerOperationPowerOn, taskcommon.OpCodePowerControlPowerOn},
		{"ForcePowerOn", PowerOperationForcePowerOn, taskcommon.OpCodePowerControlForcePowerOn},
		{"PowerOff", PowerOperationPowerOff, taskcommon.OpCodePowerControlPowerOff},
		{"ForcePowerOff", PowerOperationForcePowerOff, taskcommon.OpCodePowerControlForcePowerOff},
		{"Restart", PowerOperationRestart, taskcommon.OpCodePowerControlRestart},
		{"ForceRestart", PowerOperationForceRestart, taskcommon.OpCodePowerControlForceRestart},
		{"WarmReset", PowerOperationWarmReset, taskcommon.OpCodePowerControlWarmReset},
		{"ColdReset", PowerOperationColdReset, taskcommon.OpCodePowerControlColdReset},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.operation.CodeString()
			if got != tt.expectedCode {
				t.Errorf("PowerOperation.CodeString() = %v, want %v", got, tt.expectedCode)
			}
		})
	}
}

func TestPowerOperationFromString(t *testing.T) {
	// All known operations should round-trip through CodeString → FromString
	knownOps := []PowerOperation{
		PowerOperationPowerOn,
		PowerOperationForcePowerOn,
		PowerOperationPowerOff,
		PowerOperationForcePowerOff,
		PowerOperationRestart,
		PowerOperationForceRestart,
		PowerOperationWarmReset,
		PowerOperationColdReset,
	}

	for _, op := range knownOps {
		t.Run(op.String(), func(t *testing.T) {
			code := op.CodeString()
			got := PowerOperationFromString(code)
			if got != op {
				t.Errorf("PowerOperationFromString(%q) = %v, want %v", code, got, op)
			}
		})
	}

	// Unknown strings should return PowerOperationUnknown
	unknownCases := []string{"", "invalid", "POWER_ON", "shutdown"}
	for _, code := range unknownCases {
		t.Run("unknown_"+code, func(t *testing.T) {
			got := PowerOperationFromString(code)
			if got != PowerOperationUnknown {
				t.Errorf("PowerOperationFromString(%q) = %v, want PowerOperationUnknown", code, got)
			}
		})
	}
}

func TestFirmwareOperationCodeString(t *testing.T) {
	tests := []struct {
		name         string
		operation    FirmwareOperation
		expectedCode string
	}{
		{"Upgrade", FirmwareOperationUpgrade, taskcommon.OpCodeFirmwareControlUpgrade},
		{"Downgrade", FirmwareOperationDowngrade, taskcommon.OpCodeFirmwareControlDowngrade},
		{"Rollback", FirmwareOperationRollback, taskcommon.OpCodeFirmwareControlRollback},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.operation.CodeString()
			if got != tt.expectedCode {
				t.Errorf("FirmwareOperation.CodeString() = %v, want %v", got, tt.expectedCode)
			}
		})
	}
}

func TestExtractRuleID(t *testing.T) {
	validID := uuid.New()

	tests := []struct {
		name     string
		info     json.RawMessage
		expected *uuid.UUID
		wantErr  string
	}{
		{
			name:     "present and valid",
			info:     json.RawMessage(`{"operation":"power_on","rule_id":"` + validID.String() + `"}`),
			expected: &validID,
		},
		{
			name:     "absent",
			info:     json.RawMessage(`{"operation":"power_on"}`),
			expected: nil,
		},
		{
			name:     "empty string",
			info:     json.RawMessage(`{"operation":"power_on","rule_id":""}`),
			expected: nil,
		},
		{
			name:    "null",
			info:    json.RawMessage(`{"rule_id":null}`),
			wantErr: "rule_id must not be null",
		},
		{
			name:    "invalid UUID",
			info:    json.RawMessage(`{"rule_id":"not-a-uuid"}`),
			wantErr: "must be a valid non-zero UUID",
		},
		{
			name:    "zero UUID",
			info:    json.RawMessage(`{"rule_id":"00000000-0000-0000-0000-000000000000"}`),
			wantErr: "must be a valid non-zero UUID",
		},
		{
			name:    "invalid JSON",
			info:    json.RawMessage(`{broken`),
			wantErr: "decode operation info",
		},
		{
			name:    "nil input",
			info:    nil,
			wantErr: "decode operation info",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result, err := ExtractRuleID(tt.info)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				assert.Nil(t, result)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.expected, result)
		})
	}
}

func TestRuleID_RoundTrip_PowerControl(t *testing.T) {
	ruleID := uuid.New()
	info := &PowerControlTaskInfo{
		Operation: PowerOperationPowerOn,
		RuleID:    ruleID.String(),
	}

	raw, err := info.Marshal()
	assert.NoError(t, err)

	extracted, err := ExtractRuleID(raw)
	require.NoError(t, err)
	assert.NotNil(t, extracted)
	assert.Equal(t, ruleID, *extracted)
}

func TestRuleID_RoundTrip_BringUp(t *testing.T) {
	ruleID := uuid.New()
	info := &BringUpTaskInfo{RuleID: ruleID.String()}

	raw, err := info.Marshal()
	assert.NoError(t, err)

	extracted, err := ExtractRuleID(raw)
	require.NoError(t, err)
	assert.NotNil(t, extracted)
	assert.Equal(t, ruleID, *extracted)
}

func TestRuleID_RoundTrip_FirmwareControl(t *testing.T) {
	ruleID := uuid.New()
	info := &FirmwareControlTaskInfo{
		Operation: FirmwareOperationUpgrade,
		RuleID:    ruleID.String(),
	}

	raw, err := info.Marshal()
	assert.NoError(t, err)

	extracted, err := ExtractRuleID(raw)
	require.NoError(t, err)
	assert.NotNil(t, extracted)
	assert.Equal(t, ruleID, *extracted)
}

func TestRuleID_OmittedWhenEmpty(t *testing.T) {
	info := &PowerControlTaskInfo{
		Operation: PowerOperationPowerOn,
	}

	raw, err := info.Marshal()
	assert.NoError(t, err)

	extracted, err := ExtractRuleID(raw)
	require.NoError(t, err)
	assert.Nil(t, extracted)
}
