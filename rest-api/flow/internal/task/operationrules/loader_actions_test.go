// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"os"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestYAMLRuleLoader_ActionBasedConfiguration(t *testing.T) {
	// Create temporary YAML file with action-based configuration
	// Note: Must include all required operations for validation to pass
	yamlContent := `version: v1
rules:
  - name: "Power On with Actions"
    description: "Power on with action-based configuration"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: powershelf
        stage: 1
        max_parallel: 1
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0
        main_operation:
          name: PowerControl
        post_operation:
          - name: VerifyPowerStatus
            timeout: 15s
            poll_interval: 5s
            parameters:
              expected_status: "on"
          - name: VerifyReachability
            timeout: 3m
            poll_interval: 10s
            parameters:
              component_types: ["compute", "nvswitch"]
          - name: Sleep
            parameters:
              duration: 30s

      - component_type: nvswitch
        stage: 2
        max_parallel: 4
        timeout: 15m
        main_operation:
          name: PowerControl
        post_operation:
          - name: Sleep
            parameters:
              duration: 15s

      - component_type: compute
        stage: 3
        max_parallel: 8
        timeout: 20m
        pre_operation:
          - name: Sleep
            parameters:
              duration: 10s
        main_operation:
          name: PowerControl
        post_operation:
          - name: VerifyPowerStatus
            timeout: 15s
            poll_interval: 5s
            parameters:
              expected_status: "on"

  - name: "Power Off with Actions"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl

  - name: "Restart with Actions"
    operation_type: power_control
    operation: restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
`

	tmpfile, err := os.CreateTemp("", "test-action-rules-*.yaml")
	require.NoError(t, err)
	defer os.Remove(tmpfile.Name())

	_, err = tmpfile.Write([]byte(yamlContent))
	require.NoError(t, err)
	require.NoError(t, tmpfile.Close())

	// Load rules
	loader, err := NewYAMLRuleLoader(tmpfile.Name())
	require.NoError(t, err)

	rules, err := loader.Load()
	require.NoError(t, err)

	// Verify rules were loaded
	powerRules, ok := rules[common.TaskTypePowerControl]
	require.True(t, ok, "No power_control rules loaded")

	rule, ok := powerRules[SequencePowerOn]
	require.True(t, ok, "No power_on rule loaded")

	// Verify rule metadata
	assert.Equal(t, "Power On with Actions", rule.Name)

	// Verify steps
	require.Len(t, rule.RuleDefinition.Steps, 3)

	// Verify first step (powershelf)
	step0 := rule.RuleDefinition.Steps[0]
	assert.Equal(t, devicetypes.ComponentTypePowerShelf, step0.ComponentType)

	// Verify main operation
	assert.Equal(t, ActionPowerControl, step0.MainOperation.Name)

	// Verify post-operation actions
	require.Len(t, step0.PostOperation, 3)

	// Verify VerifyPowerStatus action
	verifyAction := step0.PostOperation[0]
	assert.Equal(t, ActionVerifyPowerStatus, verifyAction.Name)
	assert.Equal(t, 15*time.Second, verifyAction.Timeout)
	assert.Equal(t, 5*time.Second, verifyAction.PollInterval)
	assert.Equal(t, "on", verifyAction.Parameters[ParamExpectedStatus])

	// Verify VerifyReachability action
	reachAction := step0.PostOperation[1]
	assert.Equal(t, ActionVerifyReachability, reachAction.Name)
	assert.Equal(t, 3*time.Minute, reachAction.Timeout)
	assert.Equal(t, 10*time.Second, reachAction.PollInterval)

	// Verify Sleep action
	sleepAction := step0.PostOperation[2]
	assert.Equal(t, ActionSleep, sleepAction.Name)
	durationParam := sleepAction.Parameters[ParamDuration]
	require.NotNil(t, durationParam)
	duration, ok := durationParam.(time.Duration)
	require.True(t, ok)
	assert.Equal(t, 30*time.Second, duration)

	// Verify third step (compute) has pre-operation
	step2 := rule.RuleDefinition.Steps[2]
	assert.Equal(t, devicetypes.ComponentTypeCompute, step2.ComponentType)

	require.Len(t, step2.PreOperation, 1)

	preAction := step2.PreOperation[0]
	assert.Equal(t, ActionSleep, preAction.Name)
}

func TestYAMLRuleLoader_InvalidDurations(t *testing.T) {
	tests := []struct {
		name    string
		yaml    string
		wantErr bool
		errMsg  string
	}{
		{
			name: "invalid action timeout",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: VerifyPowerStatus
          timeout: "10xyz"
          poll_interval: 5s
          parameters:
            expected_status: "on"
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
  - name: "Restart"
    operation_type: power_control
    operation: restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
`,
			wantErr: true,
			errMsg:  "invalid timeout ('10xyz')",
		},
		{
			name: "invalid action poll_interval",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: VerifyPowerStatus
          timeout: 15s
          poll_interval: "bad"
          parameters:
            expected_status: "on"
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
  - name: "Restart"
    operation_type: power_control
    operation: restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
`,
			wantErr: true,
			errMsg:  "invalid poll_interval ('bad')",
		},
		{
			name: "invalid duration parameter",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: Sleep
          parameters:
            duration: "notaduration"
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
  - name: "Restart"
    operation_type: power_control
    operation: restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
`,
			wantErr: true,
			errMsg:  "invalid duration parameter for action 'Sleep' ('notaduration')",
		},
		{
			name: "invalid step timeout",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        timeout: "invalid"
        main_operation:
          name: PowerControl
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
  - name: "Restart"
    operation_type: power_control
    operation: restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
`,
			wantErr: true,
			errMsg:  "invalid timeout ('invalid')",
		},
		{
			name: "invalid retry initial_interval",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        retry:
          max_attempts: 3
          initial_interval: "bad"
          backoff_coefficient: 2.0
        main_operation:
          name: PowerControl
  - name: "Power Off"
    operation_type: power_control
    operation: power_off
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
  - name: "Restart"
    operation_type: power_control
    operation: restart
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: PowerControl
`,
			wantErr: true,
			errMsg:  "invalid initial_interval ('bad')",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpfile, err := os.CreateTemp("", "test-invalid-duration-*.yaml")
			require.NoError(t, err)
			defer os.Remove(tmpfile.Name())

			_, err = tmpfile.Write([]byte(tt.yaml))
			require.NoError(t, err)
			require.NoError(t, tmpfile.Close())

			loader, err := NewYAMLRuleLoader(tmpfile.Name())
			require.NoError(t, err)

			_, err = loader.Load()
			if tt.wantErr {
				require.Error(t, err)
				if tt.errMsg != "" {
					assert.ErrorContains(t, err, tt.errMsg)
				}
				return
			}

			require.NoError(t, err)
		})
	}
}

func TestYAMLRuleLoader_ActionValidation(t *testing.T) {
	tests := []struct {
		name    string
		yaml    string
		wantErr bool
		errMsg  string
	}{
		{
			name: "missing required parameter",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: VerifyPowerStatus
          parameters:
            # missing expected_status
`,
			wantErr: true,
			errMsg:  "missing required parameter",
		},
		{
			name: "missing timeout for action requiring it",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: VerifyPowerStatus
          poll_interval: 5s
          parameters:
            expected_status: "on"
          # missing timeout
`,
			wantErr: true,
			errMsg:  "requires timeout",
		},
		{
			name: "unknown action",
			yaml: `version: v1
rules:
  - name: "Invalid Rule"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: compute
        stage: 1
        max_parallel: 1
        main_operation:
          name: UnknownAction
`,
			wantErr: true,
			errMsg:  "unknown action",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpfile, err := os.CreateTemp("", "test-invalid-*.yaml")
			require.NoError(t, err)
			defer os.Remove(tmpfile.Name())

			_, err = tmpfile.Write([]byte(tt.yaml))
			require.NoError(t, err)
			require.NoError(t, tmpfile.Close())

			loader, err := NewYAMLRuleLoader(tmpfile.Name())
			require.NoError(t, err)

			_, err = loader.Load()
			if tt.wantErr {
				require.Error(t, err)
				if tt.errMsg != "" {
					assert.ErrorContains(t, err, tt.errMsg)
				}
				return
			}

			require.NoError(t, err)
		})
	}
}
