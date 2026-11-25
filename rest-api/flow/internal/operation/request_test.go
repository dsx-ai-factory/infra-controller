// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operation

import (
	"encoding/json"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
)

func TestWrapper_Clone(t *testing.T) {
	tests := map[string]Wrapper{
		"empty": {},
		"serialized information": {
			Code: "power_on",
			Info: json.RawMessage(`{"operation":1}`),
		},
	}

	for name, wrapper := range tests {
		t.Run(name, func(t *testing.T) {
			cloned := wrapper.Clone()
			require.Equal(t, wrapper, cloned)

			if len(cloned.Info) == 0 {
				return
			}

			cloned.Info[0] = 'x'
			require.NotEqual(t, wrapper.Info, cloned.Info)
		})
	}
}

func TestConflictStrategy_String(t *testing.T) {
	tests := map[string]struct {
		strategy ConflictStrategy
		want     string
	}{
		"reject":  {strategy: ConflictStrategyReject, want: "reject"},
		"queue":   {strategy: ConflictStrategyQueue, want: "queue"},
		"preempt": {strategy: ConflictStrategyPreempt, want: "preempt"},
		"unknown": {strategy: ConflictStrategy(42), want: "ConflictStrategy(42)"},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			require.Equal(t, test.want, test.strategy.String())
		})
	}
}

func TestParseConflictStrategy(t *testing.T) {
	tests := map[string]struct {
		value   string
		want    ConflictStrategy
		wantErr string
	}{
		"reject": {value: "reject", want: ConflictStrategyReject},
		"queue":  {value: "queue", want: ConflictStrategyQueue},
		"internal preempt is not a public strategy": {value: "preempt", want: ConflictStrategyReject, wantErr: `unknown conflict strategy "preempt"`},
		"unknown": {value: "wait", want: ConflictStrategyReject, wantErr: `unknown conflict strategy "wait"`},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			strategy, err := ParseConflictStrategy(test.value)

			require.Equal(t, test.want, strategy)

			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func TestRequest_Validate(t *testing.T) {
	triggerID := uuid.New()
	zeroTriggerID := uuid.Nil
	tests := map[string]struct {
		strategy    ConflictStrategy
		preserve    bool
		triggerType TriggerType
		triggerID   *uuid.UUID
		wantErr     string
	}{
		"no trigger":                            {},
		"internal preempt with preservation":    {strategy: ConflictStrategyPreempt, preserve: true},
		"internal preempt without preservation": {strategy: ConflictStrategyPreempt},
		"unknown conflict strategy":             {strategy: ConflictStrategy(42), wantErr: "unknown conflict strategy 42"},
		"queue rejects internal preservation":   {strategy: ConflictStrategyQueue, preserve: true, wantErr: "task preservation requires the preempt conflict strategy"},
		"type only": {
			triggerType: TriggerTypeAPI,
		},
		"type requiring missing ID": {
			triggerType: TriggerTypeEventRuleExecution,
			wantErr:     `trigger type "event_rule_execution" requires a non-zero trigger ID`,
		},
		"type requiring zero ID": {
			triggerType: TriggerTypeEventRuleExecution,
			triggerID:   &zeroTriggerID,
			wantErr:     `trigger type "event_rule_execution" requires a non-zero trigger ID`,
		},
		"type and ID": {
			triggerType: TriggerTypeEventRuleExecution,
			triggerID:   &triggerID,
		},
		"ID without type": {
			triggerID: &triggerID,
			wantErr:   `trigger type "" does not accept a trigger ID`,
		},
		"type not requiring ID rejects ID": {
			triggerType: TriggerTypeAPI,
			triggerID:   &triggerID,
			wantErr:     `trigger type "api" does not accept a trigger ID`,
		},
		"unknown type": {
			triggerType: "unknown",
			wantErr:     `unknown trigger type "unknown"`,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			request := &Request{
				Operation: Wrapper{Type: taskcommon.TaskTypePowerControl},
				TargetSpec: TargetSpec{Racks: []RackTarget{{
					Identifier: identifier.Identifier{Name: "rack-1"},
				}}},
				TriggerType: test.triggerType,
				TriggerID:   test.triggerID,
			}

			request.ConflictStrategy = test.strategy
			if test.preserve {
				request.SubmitOptions.PreserveConflictingTask = func(Wrapper) bool { return true }
			}
			err := request.Validate()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)

				return
			}

			require.NoError(t, err)
		})
	}
}

func TestRequest_RuntimeOptionsJSON(t *testing.T) {
	t.Run("preservation policy is neither serialized nor accepted from JSON", func(t *testing.T) {
		request := Request{SubmitOptions: SubmitOptions{PreserveConflictingTask: func(Wrapper) bool { return true }}}
		encoded, err := json.Marshal(request)
		require.NoError(t, err)
		require.NotContains(t, string(encoded), "SubmitOptions")
		var decoded Request
		err = json.Unmarshal([]byte(`{"SubmitOptions":{"PreserveConflictingTask":true}}`), &decoded)
		require.NoError(t, err)
		require.Nil(t, decoded.SubmitOptions.PreserveConflictingTask)
	})
}
