// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestRuleDefinition_CalculateWorkflowTimeout(t *testing.T) {
	tests := []struct {
		name    string
		ruleDef *RuleDefinition
		want    time.Duration
	}{
		{
			name:    "nil rule definition",
			ruleDef: nil,
			want:    0,
		},
		{
			name: "empty steps",
			ruleDef: &RuleDefinition{
				Steps: []SequenceStep{},
			},
			want: 0,
		},
		{
			name: "single stage with one step",
			ruleDef: &RuleDefinition{
				Steps: []SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						Timeout:       10 * time.Minute,
					},
				},
			},
			// 10m + 10% = 11m
			want: 11 * time.Minute,
		},
		{
			name: "single stage with multiple steps (parallel)",
			ruleDef: &RuleDefinition{
				Steps: []SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						Timeout:       10 * time.Minute,
					},
					{
						ComponentType: devicetypes.ComponentTypeNVSwitch,
						Stage:         1,
						Timeout:       15 * time.Minute, // Max in stage
					},
					{
						ComponentType: devicetypes.ComponentTypePowerShelf,
						Stage:         1,
						Timeout:       8 * time.Minute,
					},
				},
			},
			// Max(10m, 15m, 8m) = 15m, + 10% = 16.5m
			want: 16*time.Minute + 30*time.Second,
		},
		{
			name: "multiple stages (sequential)",
			ruleDef: &RuleDefinition{
				Steps: []SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypePowerShelf,
						Stage:         1,
						Timeout:       10 * time.Minute,
					},
					{
						ComponentType: devicetypes.ComponentTypeNVSwitch,
						Stage:         2,
						Timeout:       15 * time.Minute,
					},
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         3,
						Timeout:       20 * time.Minute,
					},
				},
			},
			// 10m + 15m + 20m = 45m, + 10% = 49.5m
			want: 49*time.Minute + 30*time.Second,
		},
		{
			name: "mixed parallel and sequential",
			ruleDef: &RuleDefinition{
				Steps: []SequenceStep{
					// Stage 1: 2 parallel steps
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						Timeout:       10 * time.Minute,
					},
					{
						ComponentType: devicetypes.ComponentTypeNVSwitch,
						Stage:         1,
						Timeout:       12 * time.Minute, // Max in stage 1
					},
					// Stage 2: 1 step
					{
						ComponentType: devicetypes.ComponentTypePowerShelf,
						Stage:         2,
						Timeout:       8 * time.Minute,
					},
				},
			},
			// Max(10m, 12m) + 8m = 20m, + 10% = 22m
			want: 22 * time.Minute,
		},
		{
			name: "steps with zero timeout",
			ruleDef: &RuleDefinition{
				Steps: []SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						Timeout:       0, // Zero timeout
					},
					{
						ComponentType: devicetypes.ComponentTypeNVSwitch,
						Stage:         2,
						Timeout:       10 * time.Minute,
					},
				},
			},
			// 0 + 10m = 10m, + 10% = 11m
			want: 11 * time.Minute,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.ruleDef.CalculateWorkflowTimeout()
			if got != tt.want {
				t.Errorf(
					"RuleDefinition.CalculateWorkflowTimeout() = %v, want %v",
					got,
					tt.want,
				)
			}
		})
	}
}
