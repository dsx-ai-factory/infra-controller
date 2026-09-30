// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// CoreGrpcExpansion - CoreGrpc Expansion
type CoreGrpcExpansion interface{}

// CoreGrpcInterface - interface to CoreGrpc
type CoreGrpcInterface interface {
	// List all the apis of Carbide here
	Init()
	Start()
	RegisterSubscriber() error
	CreateGrpcClient() error
	GetGrpcClient() *client.CoreGrpcClient
	UpdateGrpcClientState(err error)
	GetState() []string
	GetGrpcClientVersion() int64
	CheckConnection(ctx context.Context)
	CoreGrpcExpansion
}
