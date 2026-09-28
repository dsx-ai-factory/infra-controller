// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestFlowServerImplRejectsMalformedUUIDs(t *testing.T) {
	invalid := &pb.UUID{Id: "not-a-uuid"}
	targetSpec := &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Racks{
			Racks: &pb.RackTargets{
				Targets: []*pb.RackTarget{
					{Identifier: &pb.RackTarget_ExternalId{ExternalId: "D09"}},
				},
			},
		},
	}
	scheduledPowerOn := func(ruleID *pb.UUID) *pb.ScheduledOperation {
		return &pb.ScheduledOperation{
			Operation: &pb.ScheduledOperation_PowerOn{
				PowerOn: &pb.PowerOnRackRequest{
					TargetSpec: targetSpec,
					RuleId:     ruleID,
				},
			},
		}
	}

	tests := map[string]func(context.Context, *FlowServerImpl) error{
		"power on rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.PowerOnRack(ctx, &pb.PowerOnRackRequest{
				TargetSpec: targetSpec,
				RuleId:     invalid,
			})
			return err
		},
		"power off rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.PowerOffRack(ctx, &pb.PowerOffRackRequest{
				TargetSpec: targetSpec,
				RuleId:     invalid,
			})
			return err
		},
		"power reset rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.PowerResetRack(ctx, &pb.PowerResetRackRequest{
				TargetSpec: targetSpec,
				RuleId:     invalid,
			})
			return err
		},
		"bring-up rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.BringUpRack(ctx, &pb.BringUpRackRequest{
				TargetSpec: targetSpec,
				RuleId:     invalid,
			})
			return err
		},
		"ingest rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.IngestRack(ctx, &pb.IngestRackRequest{
				TargetSpec: targetSpec,
				RuleId:     invalid,
			})
			return err
		},
		"firmware rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.UpgradeFirmware(ctx, &pb.UpgradeFirmwareRequest{
				TargetSpec: targetSpec,
				RuleId:     invalid,
			})
			return err
		},
		"scheduled operation rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.CreateTaskSchedule(ctx, &pb.CreateTaskScheduleRequest{
				Schedule: &pb.ScheduleConfig{
					Name: "invalid-rule",
					Spec: &pb.ScheduleSpec{
						Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
						Spec: "1h",
					},
				},
				Operation: scheduledPowerOn(invalid),
			})
			return err
		},
		"schedule list rack filter": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.ListTaskSchedules(ctx, &pb.ListTaskSchedulesRequest{
				RackId: invalid,
			})
			return err
		},
		"schedule conflict operation rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.CheckScheduleConflicts(ctx, &pb.CheckScheduleConflictsRequest{
				Operation: scheduledPowerOn(invalid),
			})
			return err
		},
		"schedule conflict exclusion": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.CheckScheduleConflicts(ctx, &pb.CheckScheduleConflictsRequest{
				Operation:         scheduledPowerOn(nil),
				ExcludeScheduleId: invalid,
			})
			return err
		},
		"mixed task IDs": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.GetTasksByIDs(ctx, &pb.GetTasksByIDsRequest{
				TaskIds: []*pb.UUID{{Id: uuid.NewString()}, invalid},
			})
			return err
		},
		"component rack assignment": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.AddComponent(ctx, &pb.AddComponentRequest{
				Component: &pb.Component{RackId: invalid},
			})
			return err
		},
		"component ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.AddComponent(ctx, &pb.AddComponentRequest{
				Component: &pb.Component{
					Info: &pb.DeviceInfo{Id: invalid},
				},
			})
			return err
		},
		"component NVLink domain ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.AddComponent(ctx, &pb.AddComponentRequest{
				Component: &pb.Component{NvlDomainId: invalid},
			})
			return err
		},
		"component rack reassignment": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.PatchComponent(ctx, &pb.PatchComponentRequest{
				Id:     &pb.UUID{Id: uuid.NewString()},
				RackId: invalid,
			})
			return err
		},
		"rack patch ID": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.PatchRack(ctx, &pb.PatchRackRequest{
				Rack: &pb.Rack{
					Info: &pb.DeviceInfo{
						Id:           invalid,
						Manufacturer: "NVIDIA",
						SerialNumber: "rack-serial",
					},
				},
			})
			return err
		},
		"rack NVLink domain IDs": func(ctx context.Context, server *FlowServerImpl) error {
			_, err := server.CreateExpectedRack(ctx, &pb.CreateExpectedRackRequest{
				Rack: &pb.Rack{
					Info: &pb.DeviceInfo{Id: &pb.UUID{Id: uuid.NewString()}},
					NvlDomainIds: []*pb.UUID{
						{Id: uuid.NewString()},
						invalid,
					},
				},
			})
			return err
		},
		"operation run rule ID": func(ctx context.Context, server *FlowServerImpl) error {
			req := validCreateOperationRunRequest()
			req.Configuration.Operation.GetUpgradeFirmware().RuleId = invalid
			_, err := server.CreateOperationRun(ctx, req)
			return err
		},
	}

	for name, invoke := range tests {
		t.Run(name, func(t *testing.T) {
			manager := &firmwareTaskManager{}
			server := &FlowServerImpl{taskManager: manager}

			err := invoke(context.Background(), server)

			require.Equal(t, codes.InvalidArgument, status.Code(err))
			require.Nil(t, manager.request)
		})
	}
}
