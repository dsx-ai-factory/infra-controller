// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flow_test

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	flow "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// TestDecommissionRackRequest_MarshalRoundTrip verifies that a populated
// DecommissionRackRequest can be marshaled to protobuf bytes and unmarshaled
// back without data loss or a panic (the manually-patched version used a
// wrong msgTypes slot that caused a SIGSEGV during marshal).
func TestDecommissionRackRequest_MarshalRoundTrip(t *testing.T) {
	original := &flow.DecommissionRackRequest{
		TargetSpec: &flow.OperationTargetSpec{
			Targets: &flow.OperationTargetSpec_Racks{
				Racks: &flow.RackTargets{
					Targets: []*flow.RackTarget{
						{Identifier: &flow.RackTarget_Id{Id: &flow.UUID{Id: "rack-uuid-1234"}}},
					},
				},
			},
		},
		Description: "test decommission",
		QueueOptions: &flow.QueueOptions{
			ConflictStrategy: flow.ConflictStrategy_CONFLICT_STRATEGY_REJECT,
		},
		RuleId: &flow.UUID{Id: "rule-uuid-5678"},
	}

	b, err := proto.Marshal(original)
	require.NoError(t, err, "marshal must not error or panic")
	require.NotEmpty(t, b, "marshal must produce non-empty bytes for a populated message")

	roundTripped := &flow.DecommissionRackRequest{}
	require.NoError(t, proto.Unmarshal(b, roundTripped))

	assert.True(t, proto.Equal(original, roundTripped), "round-tripped message must equal the original")
	assert.Equal(t, "rack-uuid-1234", roundTripped.GetTargetSpec().GetRacks().GetTargets()[0].GetId().GetId())
	assert.Equal(t, "test decommission", roundTripped.GetDescription())
	assert.Equal(t, flow.ConflictStrategy_CONFLICT_STRATEGY_REJECT, roundTripped.GetQueueOptions().GetConflictStrategy())
	assert.Equal(t, "rule-uuid-5678", roundTripped.GetRuleId().GetId())
}
