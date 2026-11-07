// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestTriggerTypeFromString(t *testing.T) {
	tests := map[string]struct {
		value   string
		want    TriggerType
		wantErr string
	}{
		"empty": {
			want: TriggerTypeUnspecified,
		},
		"API": {
			value: "api",
			want:  TriggerTypeAPI,
		},
		"event rule execution": {
			value: "event_rule_execution",
			want:  TriggerTypeEventRuleExecution,
		},
		"unknown": {
			value:   "unknown",
			wantErr: `unknown trigger type "unknown"`,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			actual, err := TriggerTypeFromString(test.value)
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				require.Equal(t, TriggerTypeUnspecified, actual)
				return
			}

			require.NoError(t, err)
			require.Equal(t, test.want, actual)
		})
	}
}

func TestValidateTrigger(t *testing.T) {
	triggerID := uuid.New()
	zeroTriggerID := uuid.Nil
	tests := map[string]struct {
		triggerType TriggerType
		triggerID   *uuid.UUID
		wantErr     string
	}{
		"unspecified": {},
		"API": {
			triggerType: TriggerTypeAPI,
		},
		"event rule execution": {
			triggerType: TriggerTypeEventRuleExecution,
			triggerID:   &triggerID,
		},
		"unspecified with ID": {
			triggerID: &triggerID,
			wantErr:   `trigger type "" does not accept a trigger ID`,
		},
		"API with ID": {
			triggerType: TriggerTypeAPI,
			triggerID:   &triggerID,
			wantErr:     `trigger type "api" does not accept a trigger ID`,
		},
		"event rule execution without ID": {
			triggerType: TriggerTypeEventRuleExecution,
			wantErr:     `trigger type "event_rule_execution" requires a non-zero trigger ID`,
		},
		"event rule execution with zero ID": {
			triggerType: TriggerTypeEventRuleExecution,
			triggerID:   &zeroTriggerID,
			wantErr:     `trigger type "event_rule_execution" requires a non-zero trigger ID`,
		},
		"unknown": {
			triggerType: "unknown",
			wantErr:     `unknown trigger type "unknown"`,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := ValidateTrigger(test.triggerType, test.triggerID)
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
		})
	}
}

func TestTriggerType_Validate(t *testing.T) {
	tests := map[string]struct {
		triggerType TriggerType
		wantErr     string
	}{
		"unspecified": {},
		"API": {
			triggerType: TriggerTypeAPI,
		},
		"event rule execution": {
			triggerType: TriggerTypeEventRuleExecution,
		},
		"unknown": {
			triggerType: "unknown",
			wantErr:     `unknown trigger type "unknown"`,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.triggerType.Validate()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)

				return
			}

			require.NoError(t, err)
		})
	}
}

func TestTriggerType_IsSpecified(t *testing.T) {
	tests := map[string]struct {
		triggerType TriggerType
		want        bool
	}{
		"unspecified": {},
		"API": {
			triggerType: TriggerTypeAPI,
			want:        true,
		},
		"event rule execution": {
			triggerType: TriggerTypeEventRuleExecution,
			want:        true,
		},
		"unknown": {
			triggerType: "unknown",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, test.triggerType.IsSpecified())
		})
	}
}

func TestTriggerType_NeedTriggerID(t *testing.T) {
	tests := map[string]struct {
		triggerType TriggerType
		want        bool
	}{
		"unspecified": {},
		"API": {
			triggerType: TriggerTypeAPI,
		},
		"event rule execution": {
			triggerType: TriggerTypeEventRuleExecution,
			want:        true,
		},
		"unknown": {
			triggerType: "unknown",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, test.triggerType.NeedTriggerID())
		})
	}
}
