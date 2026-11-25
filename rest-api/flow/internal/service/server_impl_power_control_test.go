// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestFlowServerImpl_ACPowerCycleRack(t *testing.T) {
	manager := &powerControlTaskManager{}
	server := &FlowServerImpl{taskManager: manager}
	targetID := uuid.New()

	response, err := server.ACPowerCycleRack(context.Background(), &pb.ACPowerCycleRackRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Components{
				Components: &pb.ComponentTargets{
					Targets: []*pb.ComponentTarget{{
						Identifier: &pb.ComponentTarget_Id{Id: &pb.UUID{Id: targetID.String()}},
					}},
				},
			},
		},
		Description:            "AC power cycle test",
		OverrideReadinessCheck: true,
		ExecutionMode:          pb.ExecutionMode_EXECUTION_MODE_BEST_EFFORT,
	})

	require.NoError(t, err)
	require.Len(t, response.GetTaskIds(), 1)
	require.NotNil(t, manager.request)
	require.Equal(t, taskcommon.ExecutionModeBestEffort, manager.request.ExecutionMode)
	assert.Equal(t, "AC power cycle test", manager.request.Description)

	var info operations.PowerControlTaskInfo
	require.NoError(t, info.Unmarshal(manager.request.Operation.Info))
	assert.Equal(t, operations.PowerOperationColdReset, info.Operation)
	assert.False(t, info.Forced)
	assert.True(t, info.OverrideReadinessCheck)
}

type powerControlTaskManager struct {
	request *operation.Request
}

func (*powerControlTaskManager) Start(context.Context) error { return nil }
func (*powerControlTaskManager) Stop(context.Context)        {}
func (m *powerControlTaskManager) SubmitTask(
	_ context.Context,
	request *operation.Request,
) ([]uuid.UUID, error) {
	m.request = request
	return []uuid.UUID{uuid.New()}, nil
}
func (*powerControlTaskManager) CancelTask(context.Context, uuid.UUID) error { return nil }

func TestExecutionModeFromProto(t *testing.T) {
	for _, tc := range []struct {
		name    string
		value   pb.ExecutionMode
		want    taskcommon.ExecutionMode
		invalid bool
	}{
		{name: "omitted defaults strict", want: taskcommon.ExecutionModeStrict},
		{name: "best effort", value: pb.ExecutionMode_EXECUTION_MODE_BEST_EFFORT, want: taskcommon.ExecutionModeBestEffort},
		{name: "strict", value: pb.ExecutionMode_EXECUTION_MODE_STRICT, want: taskcommon.ExecutionModeStrict},
		{name: "unknown rejected", value: pb.ExecutionMode(99), invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := executionModeFromProto(tc.value)
			if tc.invalid {
				require.Equal(t, codes.InvalidArgument, status.Code(err))
				return
			}
			require.NoError(t, err)
			require.Equal(t, tc.want, got)
		})
	}
}
