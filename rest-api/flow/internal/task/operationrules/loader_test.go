// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestYAMLRuleLoader_InvalidOperations(t *testing.T) {
	tests := []struct {
		name        string
		yamlContent string
		wantErrMsg  string
	}{
		{
			name: "invalid power control operation",
			yamlContent: `version: v1
rules:
  - name: "Test Invalid Power Op"
    operation_type: power_control
    operation: invalid_operation_xyz
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
`,
			wantErrMsg: "invalid operation code 'invalid_operation_xyz' for operation type 'power_control'",
		},
		{
			name: "invalid firmware control operation",
			yamlContent: `version: v1
rules:
  - name: "Test Invalid Firmware Op"
    operation_type: firmware_control
    operation: bad_firmware_op
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
`,
			wantErrMsg: "invalid operation code 'bad_firmware_op' for operation type 'firmware_control'",
		},
		{
			name: "power operation for firmware type",
			yamlContent: `version: v1
rules:
  - name: "Test Wrong Type"
    operation_type: firmware_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
`,
			wantErrMsg: "invalid operation code 'power_on' for operation type 'firmware_control'",
		},
		{
			name: "firmware operation for power type",
			yamlContent: `version: v1
rules:
  - name: "Test Wrong Type"
    operation_type: power_control
    operation: upgrade
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
`,
			wantErrMsg: "invalid operation code 'upgrade' for operation type 'power_control'",
		},
		{
			name: "empty operation name",
			yamlContent: `version: v1
rules:
  - name: "Test Empty Operation"
    operation_type: power_control
    operation: ""
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
`,
			wantErrMsg: "invalid operation code '' for operation type 'power_control'",
		},
		{
			name: "multiple rules with one invalid",
			yamlContent: `version: v1
rules:
  - name: "Valid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
  - name: "Invalid Rule"
    operation_type: power_control
    operation: invalid_op
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
`,
			wantErrMsg: "invalid operation code 'invalid_op' for operation type 'power_control'",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			yamlPath := filepath.Join(t.TempDir(), "test-rules.yaml")
			require.NoError(t, os.WriteFile(yamlPath, []byte(tt.yamlContent), 0644))

			loader, err := NewYAMLRuleLoader(yamlPath)
			require.NoError(t, err)

			_, err = loader.Load()
			require.Error(t, err)
			assert.ErrorContains(t, err, tt.wantErrMsg)
		})
	}
}

func TestYAMLRuleLoader_ValidOperations(t *testing.T) {
	tests := []struct {
		name        string
		yamlContent string
		wantRules   map[common.TaskType][]string // operation type -> list of operation names
	}{
		{
			name: "valid power control operations",
			yamlContent: `version: v1
rules:
  - name: "Power On"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
  - name: "Force Restart"
    operation_type: power_control
    operation: force_restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
`,
			wantRules: map[common.TaskType][]string{
				common.TaskTypePowerControl: {"power_on", "power_off", "force_restart"},
			},
		},
		{
			name: "valid firmware control operations",
			yamlContent: `version: v1
rules:
  - name: "Upgrade"
    operation_type: firmware_control
    operation: upgrade
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: FirmwareControl
  - name: "Downgrade"
    operation_type: firmware_control
    operation: downgrade
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: FirmwareControl
  - name: "Rollback"
    operation_type: firmware_control
    operation: rollback
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: FirmwareControl
`,
			wantRules: map[common.TaskType][]string{
				common.TaskTypeFirmwareControl: {"upgrade", "downgrade", "rollback"},
			},
		},
		{
			name: "mixed valid operations",
			yamlContent: `version: v1
rules:
  - name: "Power On"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
  - name: "Upgrade"
    operation_type: firmware_control
    operation: upgrade
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: FirmwareControl
`,
			wantRules: map[common.TaskType][]string{
				common.TaskTypePowerControl:    {"power_on", "power_off"},
				common.TaskTypeFirmwareControl: {"upgrade"},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			yamlPath := filepath.Join(t.TempDir(), "test-rules.yaml")
			require.NoError(t, os.WriteFile(yamlPath, []byte(tt.yamlContent), 0644))

			loader, err := NewYAMLRuleLoader(yamlPath)
			require.NoError(t, err)

			rules, err := loader.Load()
			require.NoError(t, err)

			for opType, expectedOps := range tt.wantRules {
				typeRules, ok := rules[opType]
				require.True(t, ok, "expected rules for operation type %v", opType)
				assert.Len(t, typeRules, len(expectedOps))
				for _, opName := range expectedOps {
					assert.Contains(t, typeRules, opName, "expected rule for operation %q under type %v", opName, opType)
				}
			}
		})
	}
}
