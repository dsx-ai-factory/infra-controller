// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// FactoryResetSwitches submits NVOS or SDN reset work through the existing rack conflict
// scheduler. Core owns action support and hardware execution.
func (rs *FlowServerImpl) FactoryResetSwitches(ctx context.Context, req *pb.FactoryResetSwitchesRequest) (*pb.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Error(codes.InvalidArgument, "target_spec is required")
	}
	if rs.taskManager == nil {
		return nil, status.Error(codes.Unavailable, "task manager is not available")
	}
	info := &operations.ComponentResetTaskInfo{Target: req.GetResetTarget()}
	info.Target = info.ResetTarget()
	err := info.Validate()
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	request, err := rs.convertTargetSpecToOperationRequest(req.TargetSpec, req.Description, info)
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	ids, err := rs.taskManager.SubmitTask(ctx, request)
	if err != nil {
		if errors.Is(err, taskmanager.ErrRackConflict) {
			return nil, status.Error(codes.Aborted, "a conflicting rack task prevents factory reset")
		}
		return nil, err
	}
	if len(ids) == 0 {
		return nil, status.Error(codes.FailedPrecondition, "no switch reset tasks created")
	}
	return &pb.SubmitTaskResponse{TaskIds: protobuf.UUIDsTo(ids)}, nil
}
