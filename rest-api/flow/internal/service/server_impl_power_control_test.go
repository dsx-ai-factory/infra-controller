// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestFlowServerImpl_PowerControl(t *testing.T) {
	target := &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Components{Components: &pb.ComponentTargets{
			Targets: []*pb.ComponentTarget{{Identifier: &pb.ComponentTarget_Id{Id: &pb.UUID{Id: uuid.NewString()}}}},
		}},
	}
	cases := []struct {
		name     string
		submit   func(*FlowServerImpl) (*pb.SubmitTaskResponse, error)
		want     operations.PowerOperation
		override bool
		bypass   bool
	}{
		{
			name: "PowerOnRack",
			submit: func(server *FlowServerImpl) (*pb.SubmitTaskResponse, error) {
				return server.PowerOnRack(context.Background(), &pb.PowerOnRackRequest{
					TargetSpec: target, Description: "power test", BypassStateController: true,
				})
			},
			want:   operations.PowerOperationPowerOn,
			bypass: true,
		},
		{
			name: "PowerOffRack",
			submit: func(server *FlowServerImpl) (*pb.SubmitTaskResponse, error) {
				return server.PowerOffRack(context.Background(), &pb.PowerOffRackRequest{
					TargetSpec: target, Description: "power test", BypassStateController: true,
				})
			},
			want:   operations.PowerOperationPowerOff,
			bypass: true,
		},
		{
			name: "PowerResetRack",
			submit: func(server *FlowServerImpl) (*pb.SubmitTaskResponse, error) {
				return server.PowerResetRack(context.Background(), &pb.PowerResetRackRequest{
					TargetSpec: target, Description: "power test", BypassStateController: true,
				})
			},
			want:   operations.PowerOperationRestart,
			bypass: true,
		},
		{
			name: "ACPowerCycleRack",
			submit: func(server *FlowServerImpl) (*pb.SubmitTaskResponse, error) {
				return server.ACPowerCycleRack(context.Background(), &pb.ACPowerCycleRackRequest{
					TargetSpec: target, Description: "power test", BypassStateController: true,
				})
			},
			want:   operations.PowerOperationColdReset,
			bypass: true,
		},
		{
			name: "ACPowerCycleRack readiness override",
			submit: func(server *FlowServerImpl) (*pb.SubmitTaskResponse, error) {
				return server.ACPowerCycleRack(context.Background(), &pb.ACPowerCycleRackRequest{
					TargetSpec: target, Description: "power test", OverrideReadinessCheck: true,
				})
			},
			want:     operations.PowerOperationColdReset,
			override: true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			manager := &powerControlTaskManager{}
			server := &FlowServerImpl{taskManager: manager}
			response, err := tc.submit(server)
			require.NoError(t, err)
			require.Len(t, response.GetTaskIds(), 1)
			require.NotNil(t, manager.request)
			assert.Equal(t, "power test", manager.request.Description)
			var info operations.PowerControlTaskInfo
			require.NoError(t, info.Unmarshal(manager.request.Operation.Info))
			assert.Equal(t, tc.want, info.Operation)
			assert.False(t, info.Forced)
			assert.Equal(t, tc.override, info.OverrideReadinessCheck)
			assert.Equal(t, tc.bypass, info.BypassStateController)
		})
	}
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
