// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestActionConfig_Validate(t *testing.T) {
	tests := []struct {
		name    string
		config  ActionConfig
		wantErr bool
		errMsg  string
	}{
		{
			name: "valid Sleep action",
			config: ActionConfig{
				Name: ActionSleep,
				Parameters: map[string]any{
					ParamDuration: "30s",
				},
			},
			wantErr: false,
		},
		{
			name: "Sleep missing duration",
			config: ActionConfig{
				Name:       ActionSleep,
				Parameters: map[string]any{},
			},
			wantErr: true,
			errMsg:  "missing required parameter: duration",
		},
		{
			name: "valid VerifyPowerStatus action",
			config: ActionConfig{
				Name:         ActionVerifyPowerStatus,
				Timeout:      15 * time.Second,
				PollInterval: 5 * time.Second,
				Parameters: map[string]any{
					ParamExpectedStatus: "on",
				},
			},
			wantErr: false,
		},
		{
			name: "VerifyPowerStatus missing timeout",
			config: ActionConfig{
				Name:         ActionVerifyPowerStatus,
				PollInterval: 5 * time.Second,
				Parameters: map[string]any{
					ParamExpectedStatus: "on",
				},
			},
			wantErr: true,
			errMsg:  "requires timeout",
		},
		{
			name: "VerifyPowerStatus missing poll_interval",
			config: ActionConfig{
				Name:    ActionVerifyPowerStatus,
				Timeout: 15 * time.Second,
				Parameters: map[string]any{
					ParamExpectedStatus: "on",
				},
			},
			wantErr: true,
			errMsg:  "requires poll_interval",
		},
		{
			name: "VerifyPowerStatus invalid expected_status",
			config: ActionConfig{
				Name:         ActionVerifyPowerStatus,
				Timeout:      15 * time.Second,
				PollInterval: 5 * time.Second,
				Parameters: map[string]any{
					ParamExpectedStatus: "invalid",
				},
			},
			wantErr: true,
			errMsg:  "must be 'on' or 'off'",
		},
		{
			name: "valid VerifyReachability action",
			config: ActionConfig{
				Name:         ActionVerifyReachability,
				Timeout:      3 * time.Minute,
				PollInterval: 10 * time.Second,
				Parameters: map[string]any{
					ParamComponentTypes: []string{"compute", "nvswitch"},
				},
			},
			wantErr: false,
		},
		{
			name: "VerifyReachability invalid component type",
			config: ActionConfig{
				Name:         ActionVerifyReachability,
				Timeout:      3 * time.Minute,
				PollInterval: 10 * time.Second,
				Parameters: map[string]any{
					ParamComponentTypes: []string{"invalid_type"},
				},
			},
			wantErr: true,
			errMsg:  "invalid component type",
		},
		{
			name: "valid PowerControl action",
			config: ActionConfig{
				Name: ActionPowerControl,
			},
			wantErr: false,
		},
		{
			name: "unknown action",
			config: ActionConfig{
				Name: "UnknownAction",
			},
			wantErr: true,
			errMsg:  "unknown action",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.config.Validate()
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

func TestActionConfig_ValidateParameters(t *testing.T) {
	tests := []struct {
		name    string
		config  ActionConfig
		wantErr bool
		errMsg  string
	}{
		{
			name: "Sleep with time.Duration parameter",
			config: ActionConfig{
				Name: ActionSleep,
				Parameters: map[string]any{
					ParamDuration: 30 * time.Second,
				},
			},
			wantErr: false,
		},
		{
			name: "Sleep with numeric parameter",
			config: ActionConfig{
				Name: ActionSleep,
				Parameters: map[string]any{
					ParamDuration: 30.0,
				},
			},
			wantErr: false,
		},
		{
			name: "VerifyReachability with []any",
			config: ActionConfig{
				Name:         ActionVerifyReachability,
				Timeout:      3 * time.Minute,
				PollInterval: 10 * time.Second,
				Parameters: map[string]any{
					ParamComponentTypes: []any{"compute", "nvswitch"},
				},
			},
			wantErr: false,
		},
		{
			name: "VerifyReachability component_types not array",
			config: ActionConfig{
				Name:         ActionVerifyReachability,
				Timeout:      3 * time.Minute,
				PollInterval: 10 * time.Second,
				Parameters: map[string]any{
					ParamComponentTypes: "compute",
				},
			},
			wantErr: true,
			errMsg:  "must be array",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.config.Validate()
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

func TestActionConfig_ComponentTypes(t *testing.T) {
	tests := []struct {
		name    string
		config  ActionConfig
		want    []devicetypes.ComponentType
		wantErr bool
		errMsg  string
	}{
		{
			name: "no component type parameters",
			config: ActionConfig{
				Name: ActionPowerControl,
			},
			want: nil,
		},
		{
			name: "string slice",
			config: ActionConfig{
				Name: ActionVerifyReachability,
				Parameters: map[string]any{
					ParamComponentTypes: []string{"Compute", "PowerShelf"},
				},
			},
			want: []devicetypes.ComponentType{
				devicetypes.ComponentTypeCompute,
				devicetypes.ComponentTypePowerShelf,
			},
		},
		{
			name: "any slice",
			config: ActionConfig{
				Name: ActionVerifyReachability,
				Parameters: map[string]any{
					ParamComponentTypes: []any{"Compute", "NVSwitch"},
				},
			},
			want: []devicetypes.ComponentType{
				devicetypes.ComponentTypeCompute,
				devicetypes.ComponentTypeNVSwitch,
			},
		},
		{
			name: "invalid component type",
			config: ActionConfig{
				Name: ActionVerifyReachability,
				Parameters: map[string]any{
					ParamComponentTypes: []string{"missing"},
				},
			},
			wantErr: true,
			errMsg:  "invalid component type",
		},
		{
			name: "component types not array",
			config: ActionConfig{
				Name: ActionVerifyReachability,
				Parameters: map[string]any{
					ParamComponentTypes: "Compute",
				},
			},
			wantErr: true,
			errMsg:  "must be array",
		},
		{
			name: "required component types missing",
			config: ActionConfig{
				Name: ActionVerifyReachability,
			},
			wantErr: true,
			errMsg:  "missing required parameter",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := tt.config.ComponentTypes()
			if tt.wantErr {
				require.Error(t, err)
				if tt.errMsg != "" {
					assert.ErrorContains(t, err, tt.errMsg)
				}
				return
			}

			require.NoError(t, err)
			require.Equal(t, tt.want, got)
		})
	}
}
