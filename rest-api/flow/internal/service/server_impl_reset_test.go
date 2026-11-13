// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestFlowServerImpl_FactoryResetSwitches(t *testing.T) {
	for _, tc := range []struct {
		name        string
		scope       string
		missingSpec bool
		invalid     bool
		code        string
	}{
		{name: "missing reset target", invalid: true},
		{name: "explicit NVOS", scope: "nvos", code: taskcommon.OpCodeNVOSFactoryReset},
		{name: "SDN", scope: "sdn", code: taskcommon.OpCodeSDNFactoryReset},
		{name: "unknown scope", scope: "bmc", invalid: true},
		{name: "missing target", missingSpec: true, invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			manager := &firmwareTaskManager{}
			server := &FlowServerImpl{taskManager: manager}
			request := &pb.FactoryResetSwitchesRequest{ResetTarget: tc.scope}
			if !tc.missingSpec {
				request.TargetSpec = &pb.OperationTargetSpec{Targets: &pb.OperationTargetSpec_Components{Components: &pb.ComponentTargets{Targets: []*pb.ComponentTarget{{Identifier: &pb.ComponentTarget_External{External: &pb.ExternalRef{Id: "switch-1"}}}}}}}
			}
			response, err := server.FactoryResetSwitches(context.Background(), request)
			if tc.invalid {
				require.Equal(t, codes.InvalidArgument, status.Code(err))
				require.Nil(t, manager.request)
				return
			}
			require.NoError(t, err)
			require.Len(t, response.TaskIds, 1)
			require.Equal(t, taskcommon.TaskTypeComponentReset, manager.request.Operation.Type)
			require.Equal(t, tc.code, manager.request.Operation.Code)
			scope := tc.scope
			if scope == "" {
				scope = "nvos"
			}
			require.JSONEq(t, `{"target":"`+scope+`"}`, string(manager.request.Operation.Info))
		})
	}
}
