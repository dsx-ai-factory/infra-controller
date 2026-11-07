// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestOperationCodeValidateFor(t *testing.T) {
	tests := map[string]struct {
		taskType       TaskType
		code           OperationCode
		wantErr        bool
		wantErrMessage string
	}{
		"inject expectation": {
			taskType: TaskTypeInjectExpectation,
			code:     OpCodeInjectExpectation,
		},
		"power on": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlPowerOn,
		},
		"force power on": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlForcePowerOn,
		},
		"power off": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlPowerOff,
		},
		"force power off": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlForcePowerOff,
		},
		"restart": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlRestart,
		},
		"force restart": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlForceRestart,
		},
		"warm reset": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlWarmReset,
		},
		"cold reset": {
			taskType: TaskTypePowerControl,
			code:     OpCodePowerControlColdReset,
		},
		"firmware upgrade": {
			taskType: TaskTypeFirmwareControl,
			code:     OpCodeFirmwareControlUpgrade,
		},
		"firmware downgrade": {
			taskType: TaskTypeFirmwareControl,
			code:     OpCodeFirmwareControlDowngrade,
		},
		"firmware rollback": {
			taskType: TaskTypeFirmwareControl,
			code:     OpCodeFirmwareControlRollback,
		},
		"bring up": {
			taskType: TaskTypeBringUp,
			code:     OpCodeBringUp,
		},
		"ingest": {
			taskType: TaskTypeBringUp,
			code:     OpCodeIngest,
		},
		"mismatched task type": {
			taskType: TaskTypePowerControl,
			code:     OpCodeFirmwareControlUpgrade,
			wantErr:  true,
		},
		"unknown task type": {
			taskType:       TaskTypeUnknown,
			code:           OpCodePowerControlPowerOn,
			wantErr:        true,
			wantErrMessage: `task type "unknown" is invalid`,
		},
		"empty code": {
			taskType: TaskTypePowerControl,
			wantErr:  true,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.code.ValidateFor(test.taskType)
			if test.wantErr {
				if test.wantErrMessage != "" {
					require.EqualError(t, err, test.wantErrMessage)
					return
				}
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}
