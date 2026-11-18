// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"

	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

type recordingPowerControlClient struct {
	pb.FlowClient
	resetRequest        *pb.PowerResetRackRequest
	acPowerCycleRequest *pb.ACPowerCycleRackRequest
}

func (c *recordingPowerControlClient) PowerResetRack(
	_ context.Context,
	req *pb.PowerResetRackRequest,
	_ ...grpc.CallOption,
) (*pb.SubmitTaskResponse, error) {
	c.resetRequest = req
	return &pb.SubmitTaskResponse{}, nil
}

func (c *recordingPowerControlClient) ACPowerCycleRack(
	_ context.Context,
	req *pb.ACPowerCycleRackRequest,
	_ ...grpc.CallOption,
) (*pb.SubmitTaskResponse, error) {
	c.acPowerCycleRequest = req
	return &pb.SubmitTaskResponse{}, nil
}

func TestExecutePowerControlRoutesResetOperations(t *testing.T) {
	tests := map[string]struct {
		operation types.PowerControlOp
		wantReset bool
		wantAC    bool
	}{
		"warm reset": {
			operation: types.PowerControlOpWarmReset,
			wantReset: true,
		},
		"cold reset": {
			operation: types.PowerControlOpColdReset,
			wantAC:    true,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			recorder := &recordingPowerControlClient{}
			client := &Client{client: recorder}

			_, err := client.executePowerControl(
				context.Background(),
				&pb.OperationTargetSpec{},
				test.operation,
			)

			require.NoError(t, err)
			assert.Equal(t, test.wantReset, recorder.resetRequest != nil)
			assert.Equal(t, test.wantAC, recorder.acPowerCycleRequest != nil)
		})
	}
}
