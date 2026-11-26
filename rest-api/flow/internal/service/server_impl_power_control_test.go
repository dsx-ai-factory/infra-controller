// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
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
	})

	require.NoError(t, err)
	require.Len(t, response.GetTaskIds(), 1)
	require.NotNil(t, manager.request)
	assert.Equal(t, "AC power cycle test", manager.request.Description)

	var info operations.PowerControlTaskInfo
	require.NoError(t, info.Unmarshal(manager.request.Operation.Info))
	assert.Equal(t, operations.PowerOperationColdReset, info.Operation)
	assert.False(t, info.Forced)
	assert.True(t, info.OverrideReadinessCheck)
}

type powerControlTaskManager struct {
	err     error
	request *operation.Request
}

func (*powerControlTaskManager) Start(context.Context) error { return nil }
func (*powerControlTaskManager) Stop(context.Context)        {}
func (m *powerControlTaskManager) SubmitTask(
	_ context.Context,
	request *operation.Request,
) ([]uuid.UUID, error) {
	m.request = request
	if m.err != nil {
		return nil, m.err
	}
	return []uuid.UUID{uuid.New()}, nil
}
func (*powerControlTaskManager) CancelTask(context.Context, uuid.UUID) error { return nil }

func TestFlowServerImpl_PowerOnRack(t *testing.T) {
	tests := []struct {
		name  string
		cause error
		code  codes.Code
	}{
		{name: "rack conflict", cause: taskmanager.ErrRackConflict, code: codes.FailedPrecondition},
		{name: "queue full", cause: taskmanager.ErrQueueFull, code: codes.ResourceExhausted},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := flowRPCClient(t, &FlowServerImpl{taskManager: &powerControlTaskManager{err: tt.cause}})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			response, err := c.PowerOnRack(ctx, &pb.PowerOnRackRequest{TargetSpec: &pb.OperationTargetSpec{Targets: &pb.OperationTargetSpec_Components{Components: &pb.ComponentTargets{Targets: []*pb.ComponentTarget{{Identifier: &pb.ComponentTarget_Id{Id: &pb.UUID{Id: uuid.NewString()}}}}}}}})
			require.Equal(t, tt.code, status.Code(err))
			require.Nil(t, response)
		})
	}
}
