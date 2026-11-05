// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// ResetSwitches submits fixed NVOS reset work through the existing rack conflict
// scheduler. Core owns action support and hardware execution.
func (rs *FlowServerImpl) ResetSwitches(ctx context.Context, req *pb.ResetSwitchesRequest) (*pb.SubmitTaskResponse, error) {
	if req == nil || req.TargetSpec == nil {
		return nil, status.Error(codes.InvalidArgument, "target_spec is required")
	}
	if rs.taskManager == nil {
		return nil, status.Error(codes.Unavailable, "task manager is not available")
	}
	request, err := rs.convertTargetSpecToOperationRequest(req.TargetSpec, req.Description, &operations.ComponentResetTaskInfo{})
	if err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	ids, err := rs.taskManager.SubmitTask(ctx, request)
	if err != nil {
		return nil, err
	}
	if len(ids) == 0 {
		return nil, status.Error(codes.FailedPrecondition, "no switch reset tasks created")
	}
	return &pb.SubmitTaskResponse{TaskIds: protobuf.UUIDsTo(ids)}, nil
}
