// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package taskschedule

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// mustMarshalTemplate is a test helper that marshals a template and panics on error.
func mustMarshalTemplate(opType taskcommon.TaskType, code string, info json.RawMessage) json.RawMessage {
	raw, err := MarshalTemplate(opType, code, info, TemplateOptions{})
	if err != nil {
		panic(err)
	}
	return raw
}

func TestMarshalTemplate(t *testing.T) {
	pci := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
		Forced:    false,
	}
	info, err := pci.Marshal()
	require.NoError(t, err)

	raw, err := MarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlPowerOn, info, TemplateOptions{})
	require.NoError(t, err)

	// Round-trip: the result must be parseable and preserve all three fields.
	var tmpl TaskTemplate
	require.NoError(t, json.Unmarshal(raw, &tmpl))
	assert.Equal(t, taskcommon.TaskTypePowerControl, tmpl.Type)
	assert.Equal(t, taskcommon.OpCodePowerControlPowerOn, tmpl.Code)
	assert.JSONEq(t, string(info), string(tmpl.Info))

	// Info must also round-trip into the stored PowerControlTaskInfo payload.
	var got operations.PowerControlTaskInfo
	require.NoError(t, got.Unmarshal(tmpl.Info))
	assert.Equal(t, pci.Operation, got.Operation)
	assert.Equal(t, pci.Forced, got.Forced)
}

func TestOptionsFromTemplate(t *testing.T) {
	ruleID := "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
	info := json.RawMessage(`{}`)

	t.Run("zero options round-trip", func(t *testing.T) {
		raw, err := MarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlPowerOn, info, TemplateOptions{})
		require.NoError(t, err)

		opts, err := OptionsFromTemplate(raw)
		require.NoError(t, err)
		assert.Equal(t, 0, opts.ConflictStrategy)
		assert.Equal(t, int64(0), opts.QueueTimeoutSecs)
		assert.Equal(t, "", opts.RuleID)
	})

	t.Run("queue strategy and rule ID round-trip", func(t *testing.T) {
		raw, err := MarshalTemplate(
			taskcommon.TaskTypePowerControl,
			taskcommon.OpCodePowerControlPowerOn,
			info,
			TemplateOptions{
				ConflictStrategy: int(operation.ConflictStrategyQueue),
				QueueTimeoutSecs: 120,
				RuleID:           ruleID,
			},
		)
		require.NoError(t, err)

		opts, err := OptionsFromTemplate(raw)
		require.NoError(t, err)
		assert.Equal(t, int(operation.ConflictStrategyQueue), opts.ConflictStrategy)
		assert.Equal(t, int64(120), opts.QueueTimeoutSecs)
		assert.Equal(t, ruleID, opts.RuleID)
	})

	t.Run("invalid JSON", func(t *testing.T) {
		_, err := OptionsFromTemplate(json.RawMessage(`not-json`))
		assert.Error(t, err)
	})
}

func TestWrapperFromTemplate(t *testing.T) {
	info, err := (&operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}).Marshal()
	require.NoError(t, err)

	testCases := map[string]struct {
		input   json.RawMessage
		want    operation.Wrapper
		wantErr bool
	}{
		"valid template": {
			input: mustMarshalTemplate(
				taskcommon.TaskTypePowerControl,
				taskcommon.OpCodePowerControlPowerOn,
				info,
			),
			want: operation.Wrapper{
				Type: taskcommon.TaskTypePowerControl,
				Code: taskcommon.OpCodePowerControlPowerOn,
				Info: info,
			},
		},
		// Ingest is stored as TaskTypeBringUp + OpCodeIngest. WrapperFromTemplate
		// must preserve the "ingest" code rather than collapsing it to "bring_up".
		"ingest code preserved": {
			input: mustMarshalTemplate(
				taskcommon.TaskTypeBringUp,
				taskcommon.OpCodeIngest,
				json.RawMessage(`{}`),
			),
			want: operation.Wrapper{
				Type: taskcommon.TaskTypeBringUp,
				Code: taskcommon.OpCodeIngest,
				Info: json.RawMessage(`{}`),
			},
		},
		"invalid JSON": {
			input:   json.RawMessage(`not-json`),
			wantErr: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := WrapperFromTemplate(tc.input)
			if tc.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want.Type, got.Type)
			assert.Equal(t, tc.want.Code, got.Code)
			assert.JSONEq(t, string(tc.want.Info), string(got.Info))
		})
	}
}

func TestSummaryFromTemplate(t *testing.T) {
	emptyInfo := json.RawMessage(`{}`)

	firmwareInfo := func(version string) json.RawMessage {
		if version == "" {
			return json.RawMessage(`{}`)
		}
		raw, _ := json.Marshal(map[string]string{"target_version": version})
		return raw
	}

	testCases := map[string]struct {
		input      json.RawMessage
		wantOpType string
		wantDesc   string
		wantErr    string
	}{
		// ── error paths ───────────────────────────────────────────────────────
		"invalid JSON": {
			input:   json.RawMessage(`not-json`),
			wantErr: "unmarshal operation_template",
		},
		"firmware with non-object info": {
			// info is a JSON string, not an object — unmarshal into struct fails
			input: mustMarshalTemplate(
				taskcommon.TaskTypeFirmwareControl,
				taskcommon.OpCodeFirmwareControlUpgrade,
				json.RawMessage(`"not-an-object"`),
			),
			wantErr: "unmarshal firmware info",
		},

		// ── power control ─────────────────────────────────────────────────────
		"power_on": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlPowerOn, emptyInfo),
			wantOpType: "POWER_ON",
			wantDesc:   "Power On",
		},
		"force_power_on": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlForcePowerOn, emptyInfo),
			wantOpType: "POWER_ON",
			wantDesc:   "Power On",
		},
		"power_off": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlPowerOff, emptyInfo),
			wantOpType: "POWER_OFF",
			wantDesc:   "Power Off",
		},
		"force_power_off": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlForcePowerOff, emptyInfo),
			wantOpType: "POWER_OFF",
			wantDesc:   "Power Off (forced)",
		},
		"restart": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlRestart, emptyInfo),
			wantOpType: "POWER_RESET",
			wantDesc:   "Power Reset",
		},
		"force_restart": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, taskcommon.OpCodePowerControlForceRestart, emptyInfo),
			wantOpType: "POWER_RESET",
			wantDesc:   "Power Reset (forced)",
		},
		"unknown power control code": {
			input:      mustMarshalTemplate(taskcommon.TaskTypePowerControl, "bmc_cycle", emptyInfo),
			wantOpType: "POWER_CONTROL",
			wantDesc:   "bmc cycle",
		},

		// ── bring-up / ingest ────────────────────────────────────────────────
		"bring_up": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeBringUp, taskcommon.OpCodeBringUp, emptyInfo),
			wantOpType: "BRING_UP",
			wantDesc:   "Bring Up",
		},
		// Ingest shares TaskTypeBringUp but is distinguished solely by opcode.
		// A regression that ignores the opcode would produce "BRING_UP"/"Bring Up"
		// instead of "INGEST"/"Ingest".
		"ingest": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeBringUp, taskcommon.OpCodeIngest, emptyInfo),
			wantOpType: "INGEST",
			wantDesc:   "Ingest",
		},

		// ── firmware control ──────────────────────────────────────────────────
		"upgrade_firmware without version": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeFirmwareControl, taskcommon.OpCodeFirmwareControlUpgrade, firmwareInfo("")),
			wantOpType: "UPGRADE_FIRMWARE",
			wantDesc:   "Upgrade Firmware",
		},
		"upgrade_firmware with version": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeFirmwareControl, taskcommon.OpCodeFirmwareControlUpgrade, firmwareInfo("v2.1.0")),
			wantOpType: "UPGRADE_FIRMWARE",
			wantDesc:   "Upgrade Firmware to v2.1.0",
		},
		"downgrade_firmware without version": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeFirmwareControl, taskcommon.OpCodeFirmwareControlDowngrade, firmwareInfo("")),
			wantOpType: "DOWNGRADE_FIRMWARE",
			wantDesc:   "Downgrade Firmware",
		},
		"downgrade_firmware with version": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeFirmwareControl, taskcommon.OpCodeFirmwareControlDowngrade, firmwareInfo("v1.9.0")),
			wantOpType: "DOWNGRADE_FIRMWARE",
			wantDesc:   "Downgrade Firmware to v1.9.0",
		},
		"rollback_firmware": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeFirmwareControl, taskcommon.OpCodeFirmwareControlRollback, firmwareInfo("")),
			wantOpType: "ROLLBACK_FIRMWARE",
			wantDesc:   "Rollback Firmware",
		},
		"unknown_firmware_control_code": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeFirmwareControl, "flash", firmwareInfo("")),
			wantOpType: "FIRMWARE_CONTROL",
			wantDesc:   "flash",
		},

		// ── unknown type ──────────────────────────────────────────────────────
		"unknown operation type": {
			input:      mustMarshalTemplate(taskcommon.TaskTypeInjectExpectation, "inject", emptyInfo),
			wantOpType: "INJECT_EXPECTATION",
			wantDesc:   "Inject Expectation",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			opType, desc, err := SummaryFromTemplate(tc.input)
			if tc.wantErr != "" {
				assert.ErrorContains(t, err, tc.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.wantOpType, opType)
			assert.Equal(t, tc.wantDesc, desc)
		})
	}
}
