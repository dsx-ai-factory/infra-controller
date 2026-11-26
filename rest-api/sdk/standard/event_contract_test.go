// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestEventPolicy_UnmarshalJSON(t *testing.T) {
	for _, tc := range []struct {
		name   string
		spec   string
		ruleID string
	}{
		{"no action", `"submitTask":null,"sendAlert":null,"noAction":{"reason":""}`, ""},
		{"power Task", `"submitTask":{"targetStrategy":"Source","onConflict":"Queue","description":"","operation":{"ruleId":"63f1c140-81c9-4711-8c3a-1664aaed2cd9","powerControl":{"operation":"ForcePowerOff","overrideReadinessCheck":false},"firmwareControl":null}},"sendAlert":null,"noAction":null`, "63f1c140-81c9-4711-8c3a-1664aaed2cd9"},
		{"firmware Task", `"submitTask":{"targetStrategy":"Rack","onConflict":"Reject","description":"","operation":{"ruleId":null,"powerControl":null,"firmwareControl":{"operation":"Upgrade","targetVersion":null,"startTime":null,"endTime":null,"subTargets":null,"overrideReadinessCheck":false,"overrideVersionCheck":false}}},"sendAlert":null,"noAction":null`, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			input := `{"id":"550e8400-e29b-41d4-a716-446655440000","name":"Example","description":"","eventType":"hardware.leak.detected","enabled":true,"readOnly":false,"actions":[{"name":"handle","condition":null,` + tc.spec + `}],"created":null,"updated":null}`
			var policy EventPolicy
			err := json.Unmarshal([]byte(input), &policy)
			require.NoError(t, err)
			raw, err := json.Marshal(policy)
			require.NoError(t, err)
			var roundtrip EventPolicy
			err = json.Unmarshal(raw, &roundtrip)
			require.NoError(t, err)
			require.Len(t, roundtrip.Actions, 1)
			if tc.ruleID != "" {
				submit := roundtrip.Actions[0].GetSubmitTask()
				operation := submit.GetOperation()
				require.Equal(t, tc.ruleID, operation.GetRuleId())
			}
		})
	}
}

func TestEventHandling_UnmarshalJSON(t *testing.T) {
	const handlingID = "b9a73216-8b7a-4c89-9e6d-391e3dd15012"
	for _, tc := range []struct {
		name      string
		missingID bool
	}{
		{name: "preserves opaque handling identity"},
		{name: "requires handling identity", missingID: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			input := `{"id":"b9a73216-8b7a-4c89-9e6d-391e3dd15012","event":{"eventType":"hardware.leak.detected","detectedAt":null,"summary":"Leak detected","sourceId":"component-17","sourceType":"Component"},"appliedPolicy":{"id":"c3e4990d-b63f-42f3-93ec-a778c4975044","actions":[{"name":"record","condition":null,"submitTask":null,"sendAlert":null,"noAction":{"reason":"Observe only"}}]},"reason":null,"message":"No action was requested.","status":"NoAction","tasks":[],"handledAt":"2026-10-26T16:00:00Z"}`
			if tc.missingID {
				input = strings.Replace(input, `"id":"`+handlingID+`",`, "", 1)
			}
			var handling EventHandling
			err := json.Unmarshal([]byte(input), &handling)
			if tc.missingID {
				require.ErrorContains(t, err, "no value given for required property id")
				return
			}
			require.NoError(t, err)
			require.Equal(t, handlingID, handling.GetId())
			require.Equal(t, EVENTHANDLINGSTATUS_NO_ACTION, handling.GetStatus())
			raw, err := json.Marshal(handling)
			require.NoError(t, err)
			require.JSONEq(t, input, string(raw))
		})
	}
}
