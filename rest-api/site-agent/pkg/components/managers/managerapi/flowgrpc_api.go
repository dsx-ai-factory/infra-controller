// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

import (
	"github.com/dsx-ai-factory/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// FlowGrpcExpansion - FlowGrpc Expansion
type FlowGrpcExpansion interface{}

// FlowGrpcInterface - interface to FlowGrpc
type FlowGrpcInterface interface {
	// List all the apis of FlowGrpc here
	Init()
	Start()
	CreateGrpcClient() error
	GetGrpcClient() *client.FlowGrpcClient
	UpdateGrpcClientState(err error)
	RegisterSubscriber() error
	GetState() []string
	GetGrpcClientVersion() int64
	FlowGrpcExpansion
}
