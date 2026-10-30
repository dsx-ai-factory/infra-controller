// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec_test

import (
	"bytes"
	"os"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/stretchr/testify/require"
)

func TestPolicyV1Fixture(t *testing.T) {
	data, err := os.ReadFile("testdata/policy_v1_all_actions.json")
	require.NoError(t, err)
	tests := []struct {
		name string
		data []byte
	}{
		{name: "legacy component token", data: data},
		{name: "source token", data: bytes.ReplaceAll(data, []byte(`"targetStrategy": "component"`), []byte(`"targetStrategy": "source"`))},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			policy, err := codec.UnmarshalPolicy(test.data)
			require.NoError(t, err)
			require.Equal(t, fullPolicy(), policy)
		})
	}
}

func TestPolicyV1NoopFixture(t *testing.T) {
	data, err := os.ReadFile("testdata/policy_v1_without_dedupe.json")
	require.NoError(t, err)

	policy, err := codec.UnmarshalPolicy(data)
	require.NoError(t, err)
	require.Len(t, policy.Actions, 1)
	require.Equal(t, "noop", policy.Actions[0].Name)
}

func TestPolicyRoundTrip(t *testing.T) {
	tests := map[string]eventrule.Policy{
		"empty":        {},
		"with actions": fullPolicy(),
	}
	for name, policy := range tests {
		t.Run(name, func(t *testing.T) {
			encoded, err := codec.MarshalPolicy(policy)
			require.NoError(t, err)
			if len(policy.Actions) > 0 {
				require.Contains(t, string(encoded), `"targetStrategy":"component"`)
			}
			roundTripped, err := codec.UnmarshalPolicy(encoded)
			require.NoError(t, err)
			require.Equal(t, policy, roundTripped)
		})
	}
}

func TestPolicyRejectsUnknownVersionsAndFields(t *testing.T) {
	tests := map[string]string{
		"policy version": `{"version":2,"actions":[]}`,
		"removed dedupe field": `{
			"version":1,
			"dedupe":{"version":2,"window":1},
			"actions":[]
		}`,
		"action version": `{
			"version":1,
			"actions":[
				{"version":2,"name":"noop","type":"noop","condition":{},"spec":{}}
			]
		}`,
		"policy field": `{"version":1,"actions":[],"unknown":true}`,
	}

	for name, data := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := codec.UnmarshalPolicy([]byte(data))
			require.Error(t, err)
		})
	}
}

func fullPolicy() eventrule.Policy {
	return eventrule.Policy{
		Actions: []eventrule.Action{
			{
				Name: "submit",
				Condition: eventrule.ActionCondition{
					Severities:     []eventrule.Severity{eventrule.SeverityCritical},
					ComponentTypes: []flowtypes.ComponentType{flowtypes.ComponentTypeCompute},
				},
				Spec: &eventrule.SubmitTask{
					Operation: &operations.PowerControlTaskInfo{
						Operation: operations.PowerOperationForcePowerOff,
					},
					TargetStrategy:   eventrule.TargetStrategySource,
					ConflictStrategy: eventrule.ConflictStrategyQueue,
					Description:      "power off",
				},
			},
			{
				Name: "alert",
				Spec: &eventrule.SendAlert{
					Severity: eventrule.SeverityWarning,
					Message:  "leak detected",
				},
			},
			{Name: "noop", Spec: &eventrule.Noop{Reason: "audit only"}},
		},
	}
}
