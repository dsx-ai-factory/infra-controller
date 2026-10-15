// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpctypes

import (
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"go.uber.org/atomic"
)

// State - Flow state
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

// FlowGrpc represents the gRPC client for FlowGrpc and state
type FlowGrpc struct {
	Client *client.FlowGrpcAtomicClient
	State  *State
}

// NewFlowGrpcInstance creates a new instance of FlowGrpc
func NewFlowGrpcInstance() *FlowGrpc {
	f := &FlowGrpc{
		State:  &State{},
		Client: client.NewFlowGrpcAtomicClient(&client.FlowGrpcClientConfig{}),
	}

	return f
}

// GetClient returns the Flow client
func (c *FlowGrpc) GetClient() *client.FlowGrpcClient {
	return c.Client.GetClient()
}
