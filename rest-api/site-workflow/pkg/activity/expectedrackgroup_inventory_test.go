// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"fmt"
	"net"
	"sync/atomic"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
)

type expectedRackGroupServer struct {
	corev1.UnimplementedForgeServer
	groups []*corev1.ExpectedRackGroup
	err    error
	calls  atomic.Int32
}

func (*expectedRackGroupServer) Version(context.Context, *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{BuildVersion: "1.0.0"}, nil
}

func (server *expectedRackGroupServer) GetAllExpectedRackGroups(context.Context, *emptypb.Empty) (*corev1.ExpectedRackGroupList, error) {
	server.calls.Add(1)
	return &corev1.ExpectedRackGroupList{ExpectedRackGroups: server.groups}, server.err
}

func TestManageExpectedRackGroupInventory_DiscoverExpectedRackGroupInventory(t *testing.T) {
	for _, pageSize := range []int{0, -1} {
		t.Run(fmt.Sprintf("invalid page size %d", pageSize), func(t *testing.T) {
			manager := NewManageExpectedRackGroupInventory(uuid.New(), nil, nil, "inventory-test", pageSize)
			err := manager.DiscoverExpectedRackGroupInventory(context.Background())
			require.EqualError(t, err, fmt.Sprintf("cloud page size must be positive: %d", pageSize))
		})
	}

	for _, tc := range []struct {
		name          string
		count         int
		coreCode      codes.Code
		missingID     bool
		cancelPublish bool
	}{
		{name: "empty inventory"},
		{name: "multiple cloud pages from one Core response", count: 26},
		{name: "missing ID follows expected rack filtering", count: 1, missingID: true},
		{name: "collection failure", coreCode: codes.Unavailable},
		{name: "older Core does not support GetAll", coreCode: codes.Unimplemented},
		{name: "publication cancellation", count: 1, cancelPublish: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			server := &expectedRackGroupServer{err: status.Error(tc.coreCode, "collection failed")}
			ids := make([]string, 0, tc.count)
			for i := 0; i < tc.count; i++ {
				id := fmt.Sprintf("group-%03d", i)
				ids = append(ids, id)
				server.groups = append(server.groups, &corev1.ExpectedRackGroup{RackGroupId: &corev1.RackGroupId{Id: id}})
			}
			if tc.missingID {
				server.groups = append(server.groups, &corev1.ExpectedRackGroup{})
			}
			listener, err := net.Listen("tcp", "127.0.0.1:0")
			require.NoError(t, err)
			grpcServer := grpc.NewServer()
			corev1.RegisterForgeServer(grpcServer, server)
			serveErr := make(chan error, 1)
			go func() { serveErr <- grpcServer.Serve(listener) }()
			t.Cleanup(func() { grpcServer.Stop(); require.NoError(t, <-serveErr) })
			client, err := cclient.NewCoreGrpcClient(&cclient.CoreGrpcClientConfig{Address: listener.Addr().String()})
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, client.Close()) })
			atomicClient := cclient.NewCoreGrpcAtomicClient(&cclient.CoreGrpcClientConfig{})
			atomicClient.SwapClient(client)
			publisher := &tmocks.Client{}
			siteID := uuid.New()
			calls := 0
			var publishErr error
			if tc.cancelPublish {
				publishErr = context.Canceled
			}
			publisher.On("ExecuteWorkflow", mock.Anything, mock.Anything, "UpdateExpectedRackGroupInventory", siteID, mock.Anything).Run(func(args mock.Arguments) {
				calls++
				require.Equal(t, ctx, args.Get(0), "publication must use the activity context")
				inventory := args.Get(4).(*corev1.ExpectedRackGroupInventory)
				if tc.coreCode != codes.OK {
					require.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_FAILED, inventory.InventoryStatus)
					require.Empty(t, inventory.ExpectedRackGroups)
					return
				}
				require.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, inventory.InventoryStatus)
				require.Equal(t, ids, inventory.InventoryPage.ItemIds)
				require.EqualValues(t, tc.count, inventory.InventoryPage.TotalItems)
				require.EqualValues(t, calls, inventory.InventoryPage.CurrentPage)
				require.Len(t, inventory.ExpectedRackGroups, min(25, tc.count-(calls-1)*25))
				if tc.cancelPublish {
					cancel()
					require.ErrorIs(t, args.Get(0).(context.Context).Err(), context.Canceled)
				}
			}).Return(&tmocks.WorkflowRun{}, publishErr)
			manager := NewManageExpectedRackGroupInventory(siteID, atomicClient, publisher, "inventory-test", 25)
			err = manager.DiscoverExpectedRackGroupInventory(ctx)
			if tc.coreCode != codes.OK {
				require.Equal(t, tc.coreCode, status.Code(err))
			} else if tc.cancelPublish {
				require.ErrorIs(t, err, context.Canceled)
			} else {
				require.NoError(t, err)
			}
			require.EqualValues(t, 1, server.calls.Load())
			require.Equal(t, max(1, (tc.count+24)/25), calls)
			publisher.AssertExpectations(t)
		})
	}
}
