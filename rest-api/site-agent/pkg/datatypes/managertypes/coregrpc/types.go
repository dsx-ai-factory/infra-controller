// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpctypes

import (
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"go.uber.org/atomic"
)

// State - Core gRPC state
type State struct {
	// GrpcFail the number of times the rpc has failed
	GrpcFail atomic.Uint64
	// GrpcSucc the number of times the rpc has succeeded
	GrpcSucc atomic.Uint64
	// HealthStatus current health state
	HealthStatus atomic.Uint64
	// Err is error message
	Err atomic.String
	// WflowMetrics workflow metrics
	WflowMetrics WorkflowMetrics
}

// CoreGrpc represents the gRPC client for Core gRPC and state
type CoreGrpc struct {
	Client *client.CoreGrpcAtomicClient
	State  *State
}

// NewCoreGrpcInstance creates a new instance of Core gRPC
func NewCoreGrpcInstance() *CoreGrpc {
	coreGrpc := &CoreGrpc{
		State:  &State{},
		Client: client.NewCoreGrpcAtomicClient(&client.CoreGrpcClientConfig{}),
	}

	return coreGrpc
}

// GetClient returns the Core gRPC client
func (c *CoreGrpc) GetClient() *client.CoreGrpcClient {
	return c.Client.GetClient()
}
