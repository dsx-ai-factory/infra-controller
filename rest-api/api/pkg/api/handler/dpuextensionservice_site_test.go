// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net"
	"sync"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	siteactivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	coreclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc"
)

// Exercise the real Site activity through gRPC while the REST handler owns its
// real database transaction. Only Temporal dispatch is mocked.
type dpuDeleteCoreServer struct {
	corev1.UnimplementedForgeServer
	mu        sync.Mutex
	deleteErr error
	lookupErr error
	services  []*corev1.DpuExtensionService
	deletes   []*corev1.DeleteDpuExtensionServiceRequest
	lookups   []*corev1.DpuExtensionServicesByIdsRequest
}

func (s *dpuDeleteCoreServer) Version(context.Context, *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{}, nil
}

func (s *dpuDeleteCoreServer) DeleteDpuExtensionService(_ context.Context, request *corev1.DeleteDpuExtensionServiceRequest) (*corev1.DeleteDpuExtensionServiceResponse, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.deletes = append(s.deletes, request)
	return &corev1.DeleteDpuExtensionServiceResponse{}, s.deleteErr
}

func (s *dpuDeleteCoreServer) FindDpuExtensionServicesByIds(_ context.Context, request *corev1.DpuExtensionServicesByIdsRequest) (*corev1.DpuExtensionServiceList, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.lookups = append(s.lookups, request)
	return &corev1.DpuExtensionServiceList{Services: s.services}, s.lookupErr
}

func mockDpuDeleteSiteWorkflow(t *testing.T, server *dpuDeleteCoreServer) *tmocks.Client {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	grpcServer := grpc.NewServer()
	corev1.RegisterForgeServer(grpcServer, server)
	stopped := make(chan error, 1)
	go func() { stopped <- grpcServer.Serve(listener) }()
	t.Cleanup(func() {
		grpcServer.Stop()
		require.NoError(t, <-stopped)
	})
	client, err := coreclient.NewCoreGrpcClient(&coreclient.CoreGrpcClientConfig{
		Address: listener.Addr().String(), Secure: coreclient.InsecureGrpc,
	})
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, client.Close()) })
	atomicClient := coreclient.NewCoreGrpcAtomicClient(&coreclient.CoreGrpcClientConfig{})
	atomicClient.SwapClient(client)
	activity := siteactivity.NewManageDpuExtensionService(atomicClient)
	var request *corev1.DeleteDpuExtensionServiceRequest
	run := &tmocks.WorkflowRun{}
	run.On("GetID").Return("test-dpu-delete")
	run.On("Get", mock.Anything, mock.Anything).Return(func(ctx context.Context, _ interface{}) error {
		return activity.DeleteDpuExtensionServiceOnSite(ctx, request)
	})
	clientMock := &tmocks.Client{}
	clientMock.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteDpuExtensionService", mock.Anything).
		Run(func(args mock.Arguments) { request = args.Get(3).(*corev1.DeleteDpuExtensionServiceRequest) }).
		Return(run, nil)
	return clientMock
}
