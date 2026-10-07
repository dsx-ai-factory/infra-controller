// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec_test

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/stretchr/testify/require"
)

func TestActionRoundTrip(t *testing.T) {
	tests := map[string]eventrule.Action{
		"submit task": {
			Name: "submit",
			Condition: eventrule.ActionCondition{
				Severities:     []eventrule.Severity{eventrule.SeverityCritical},
				ComponentTypes: []flowtypes.ComponentType{flowtypes.ComponentTypeCompute},
			},
			Spec: &eventrule.SubmitTask{
				Operation: &operations.FirmwareControlTaskInfo{
					Operation:              operations.FirmwareOperationUpgrade,
					TargetVersion:          "1.2.3",
					StartTime:              123,
					EndTime:                456,
					SubTargets:             []string{"bmc", "bios"},
					OverrideReadinessCheck: true,
				},
				TargetStrategy:   eventrule.TargetStrategyComponent,
				ConflictStrategy: eventrule.ConflictStrategyQueue,
				Description:      "power off",
			},
		},
		"send alert": {
			Name: "alert",
			Spec: &eventrule.SendAlert{
				Severity: eventrule.SeverityWarning,
				Message:  "leak detected",
			},
		},
		"noop": {Name: "noop", Spec: &eventrule.Noop{Reason: "audit only"}},
	}

	for name, action := range tests {
		t.Run(name, func(t *testing.T) {
			encoded, err := codec.MarshalAction(action)
			require.NoError(t, err)

			roundTripped, err := codec.UnmarshalAction(encoded)
			require.NoError(t, err)
			require.Equal(t, action, roundTripped)
		})
	}
}

func TestMarshalActionRejectsInvalidAction(t *testing.T) {
	_, err := codec.MarshalAction(eventrule.Action{})
	require.Error(t, err)
}

func TestUnmarshalAction(t *testing.T) {
	tests := map[string]string{
		"unknown version": `{
			"version":2,
			"name":"noop",
			"type":"noop",
			"condition":{},
			"spec":{}
		}`,
		"unknown action field": `{
			"version":1,
			"name":"noop",
			"type":"noop",
			"condition":{},
			"spec":{},
			"unknown":true
		}`,
		"invalid condition severity": `{
			"version":1,
			"name":"noop",
			"type":"noop",
			"condition":{"severities":["urgent"]},
			"spec":{}
		}`,
		"invalid condition component type": `{
			"version":1,
			"name":"noop",
			"type":"noop",
			"condition":{"componentTypes":["GPU"]},
			"spec":{}
		}`,
		"invalid send alert severity": `{
			"version":1,
			"name":"alert",
			"type":"send_alert",
			"condition":{},
			"spec":{"severity":"urgent"}
		}`,
		"unspecified send alert severity": `{
			"version":1,
			"name":"alert",
			"type":"send_alert",
			"condition":{},
			"spec":{"severity":""}
		}`,
		"unknown action type": `{
			"version":1,
			"name":"unknown",
			"type":"unknown",
			"condition":{},
			"spec":{}
		}`,
		"operation code does not match payload": `{
			"version":1,
			"name":"submit",
			"type":"submit_task",
			"condition":{},
			"spec":{
				"operation":{
					"type":"power_control",
					"code":"power_on",
					"payload":{"operation":4,"forced":true}
				},
				"targetStrategy":"rack",
				"conflictStrategy":"queue"
			}
		}`,
		"invalid operation payload enum": `{
			"version":1,
			"name":"submit",
			"type":"submit_task",
			"condition":{},
			"spec":{
				"operation":{
					"type":"power_control",
					"code":"power_on",
					"payload":{"operation":100}
				},
				"targetStrategy":"rack",
				"conflictStrategy":"queue"
			}
		}`,
	}

	for name, data := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := codec.UnmarshalAction([]byte(data))
			require.Error(t, err)
		})
	}
}
