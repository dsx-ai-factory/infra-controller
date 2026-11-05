// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestFlowServerImpl_FactoryResetSwitches(t *testing.T) {
	for _, invalid := range []bool{false, true} {
		name := "submits fixed reset"
		if invalid {
			name = "missing target"
		}
		t.Run(name, func(t *testing.T) {
			manager := &firmwareTaskManager{}
			server := &FlowServerImpl{taskManager: manager}
			request := &pb.FactoryResetSwitchesRequest{}
			if !invalid {
				request.TargetSpec = &pb.OperationTargetSpec{Targets: &pb.OperationTargetSpec_Components{Components: &pb.ComponentTargets{Targets: []*pb.ComponentTarget{{Identifier: &pb.ComponentTarget_External{External: &pb.ExternalRef{Id: "switch-1"}}}}}}}
			}
			response, err := server.FactoryResetSwitches(context.Background(), request)
			require.Equal(t, invalid, err != nil)
			if invalid {
				require.Nil(t, manager.request)
				return
			}
			require.Len(t, response.TaskIds, 1)
			require.Equal(t, taskcommon.TaskTypeComponentReset, manager.request.Operation.Type)
			require.Equal(t, taskcommon.OpCodeNVOSFactoryReset, manager.request.Operation.Code)
		})
	}
}
