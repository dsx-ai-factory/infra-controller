// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// firmwareDetailsForgeClient records the passive RPC and its effective deadline.
// Embedding the client makes unexpected discovery or mutation calls fail loudly.
type firmwareDetailsForgeClient struct {
	corev1.ForgeClient
	request  *corev1.GetComponentFirmwareDetailsRequest
	deadline time.Time
	response *corev1.GetComponentFirmwareDetailsResponse
	err      error
}

// GetComponentFirmwareDetails captures the request and effective deadline at the transport seam.
func (c *firmwareDetailsForgeClient) GetComponentFirmwareDetails(ctx context.Context, request *corev1.GetComponentFirmwareDetailsRequest, _ ...grpc.CallOption) (*corev1.GetComponentFirmwareDetailsResponse, error) {
	c.request = request
	c.deadline, _ = ctx.Deadline()
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	if c.err != nil {
		return nil, c.err
	}
	return c.response, c.err
}

// TestGrpcClient_GetComponentFirmwareDetails keeps the shared connection and Core
// messages intact while respecting both configured and caller timeout budgets.
func TestGrpcClient_GetComponentFirmwareDetails(t *testing.T) {
	tests := []struct {
		name          string
		callerTimeout time.Duration
		clientTimeout time.Duration
		coreError     error
	}{
		// The service's shorter budget must survive the provider default.
		{name: "caller deadline", callerTimeout: time.Second, clientTimeout: time.Minute},
		// Operator configuration can make a call shorter, never longer.
		{name: "configured deadline", callerTimeout: time.Minute, clientTimeout: time.Second, coreError: status.Error(codes.Unimplemented, "unsupported")},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(t.Context(), test.callerTimeout)
			defer cancel()
			response := &corev1.GetComponentFirmwareDetailsResponse{}
			forge := &firmwareDetailsForgeClient{response: response, err: test.coreError}
			client := &grpcClient{gclient: newBatchingForgeClient(forge), grpcTimeout: test.clientTimeout}
			request := &corev1.GetComponentFirmwareDetailsRequest{Target: &corev1.GetComponentFirmwareDetailsRequest_MachineIds{
				MachineIds: &corev1.MachineIdList{MachineIds: []*corev1.MachineId{{Id: "managed-host"}}},
			}}
			started := time.Now()

			actual, err := client.GetComponentFirmwareDetails(ctx, request)

			require.Same(t, request, forge.request)
			require.Equal(t, test.coreError, err)
			if test.coreError != nil {
				require.Nil(t, actual)
			} else {
				require.Same(t, response, actual)
			}

			callerDeadline, _ := ctx.Deadline()
			if test.callerTimeout < test.clientTimeout {
				require.Equal(t, callerDeadline, forge.deadline)
			} else {
				require.False(t, forge.deadline.Before(started.Add(test.clientTimeout)))
				require.False(t, forge.deadline.After(time.Now().Add(test.clientTimeout)))
				require.True(t, forge.deadline.Before(callerDeadline))
			}
		})
	}
}
