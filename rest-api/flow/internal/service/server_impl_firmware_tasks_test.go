// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// TestFirmwareTaskDetailsToProto pins every projected intent field and both
// target forms. Exact equality catches lost timestamps, false flags and defaults
// that could otherwise look like successfully reconstructed firmware policy.
func TestFirmwareTaskDetailsToProto(t *testing.T) {
	tests := []struct {
		name  string
		input taskdef.FirmwareRequestedTargets
		want  *pb.FirmwareRequestedTargets
	}{
		// Shared scalar input is independent of installed/effective expected versions.
		{name: "shared exact version", input: taskdef.FirmwareRequestedTargets{Shared: &taskdef.FirmwareTargetMetadata{Kind: taskdef.FirmwareTargetVersion, Value: " release-3 "}}, want: &pb.FirmwareRequestedTargets{Selection: &pb.FirmwareRequestedTargets_Shared{Shared: &pb.FirmwareTargetMetadata{Kind: pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_VERSION, Value: " release-3 "}}}},
		// An omitted type stays absent; an explicit default remains present and empty.
		{name: "typed manifest and default", input: taskdef.FirmwareRequestedTargets{ByType: map[devicetypes.ComponentType]taskdef.FirmwareTargetMetadata{
			devicetypes.ComponentTypeNVSwitch: {Kind: taskdef.FirmwareTargetManifest, Value: "object-4"}, devicetypes.ComponentTypeCompute: {Kind: taskdef.FirmwareTargetConfiguredDefault},
		}}, want: &pb.FirmwareRequestedTargets{Selection: &pb.FirmwareRequestedTargets_ByType{ByType: &pb.FirmwareTargetsByType{Entries: []*pb.FirmwareTargetForType{
			{ComponentType: pb.ComponentType_COMPONENT_TYPE_COMPUTE, Target: &pb.FirmwareTargetMetadata{Kind: pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_CONFIGURED_DEFAULT}},
			{ComponentType: pb.ComponentType_COMPONENT_TYPE_NVSWITCH, Target: &pb.FirmwareTargetMetadata{Kind: pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_MANIFEST, Value: "object-4"}},
		}}}}},
		// Unavailable metadata must never invent a display value from hidden input.
		{name: "unavailable metadata", input: taskdef.FirmwareRequestedTargets{Shared: &taskdef.FirmwareTargetMetadata{Kind: taskdef.FirmwareTargetUnavailable, Value: "must-stay-hidden"}}, want: &pb.FirmwareRequestedTargets{Selection: &pb.FirmwareRequestedTargets_Shared{Shared: &pb.FirmwareTargetMetadata{Kind: pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_UNAVAILABLE}}}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			started := time.Unix(20, 0)
			task := taskdef.FirmwareTaskDetails{ID: uuid.UUID{15: 1}, RackID: uuid.UUID{15: 2}, Operation: operations.FirmwareOperationDowngrade,
				Attributes:       taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {uuid.UUID{15: 3}}}},
				RequestedTargets: tc.input, SubTargets: []string{"BIOS", "BMC"}, OverrideReadinessCheck: true, OverrideVersionCheck: false, StartTime: 100, EndTime: 200,
				Status: taskcommon.TaskStatusRunning, CreatedAt: time.Unix(10, 0), UpdatedAt: time.Unix(30, 0), StartedAt: &started,
			}
			want := &pb.FirmwareTaskDetails{Id: &pb.UUID{Id: "00000000-0000-0000-0000-000000000001"}, RackId: &pb.UUID{Id: "00000000-0000-0000-0000-000000000002"}, Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE,
				Targets: &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: "00000000-0000-0000-0000-000000000003"}}}}},
				Status:  pb.TaskStatus_TASK_STATUS_RUNNING, CreatedAt: timestamppb.New(time.Unix(10, 0)), UpdatedAt: timestamppb.New(time.Unix(30, 0)), StartedAt: timestamppb.New(time.Unix(20, 0)),
				RequestedTargets: tc.want, SubTargets: []string{"BIOS", "BMC"}, OverrideReadinessCheck: true, OverrideVersionCheck: false, StartTime: proto.Int64(100), EndTime: proto.Int64(200),
			}

			got, err := firmwareTaskDetailsToProto(task)

			require.NoError(t, err)
			require.True(t, proto.Equal(want, got), "got %v; want %v", got, want)
		})
	}
}
