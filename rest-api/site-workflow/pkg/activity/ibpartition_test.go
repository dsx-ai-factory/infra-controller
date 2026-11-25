// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestManageIBPartitionInventory_DiscoverInfiniBandPartitionInventory(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	type fields struct {
		siteID               uuid.UUID
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
		temporalPublishQueue string
		sitePageSize         int
		cloudPageSize        int
	}
	type args struct {
		wantTotalItems int
	}
	tests := []struct {
		name   string
		fields fields
		args   args
	}{
		{
			name: "test collecting and publishing ib partition inventory, empty inventory",
			fields: fields{
				siteID:               uuid.New(),
				coreGrpcAtomicClient: coreGrpcAtomicClient,
				temporalPublishQueue: "test-queue",
				sitePageSize:         100,
				cloudPageSize:        25,
			},
			args: args{
				wantTotalItems: 0,
			},
		},
		{
			name: "test collecting and publishing ib partition inventory, normal inventory",
			fields: fields{
				siteID:               uuid.New(),
				coreGrpcAtomicClient: coreGrpcAtomicClient,
				temporalPublishQueue: "test-queue",
				sitePageSize:         100,
				cloudPageSize:        25,
			},
			args: args{
				wantTotalItems: 195,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tc := &tmocks.Client{}
			tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
				mock.AnythingOfType("string"), mock.AnythingOfType("uuid.UUID"), mock.Anything).Return(wrun, nil)
			tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)

			manageInstance := NewManageInfiniBandPartitionInventory(ManageInventoryConfig{
				SiteID:                tt.fields.siteID,
				CoreGrpcAtomicClient:  tt.fields.coreGrpcAtomicClient,
				TemporalPublishClient: tc,
				TemporalPublishQueue:  tt.fields.temporalPublishQueue,
				SitePageSize:          tt.fields.sitePageSize,
				CloudPageSize:         tt.fields.cloudPageSize,
			})

			ctx := context.Background()
			ctx = context.WithValue(ctx, cClient.MockWantCount, tt.args.wantTotalItems)

			totalPages := tt.args.wantTotalItems / tt.fields.cloudPageSize
			if tt.args.wantTotalItems%tt.fields.cloudPageSize > 0 {
				totalPages++
			}

			err := manageInstance.DiscoverInfiniBandPartitionInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.InfiniBandPartitionInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.IbPartitions))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.IbPartitions))
			}

			assert.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, inventory.InventoryStatus)
			assert.Equal(t, totalPages, int(inventory.InventoryPage.TotalPages))
			assert.Equal(t, 1, int(inventory.InventoryPage.CurrentPage))
			assert.Equal(t, tt.fields.cloudPageSize, int(inventory.InventoryPage.PageSize))
			assert.Equal(t, tt.args.wantTotalItems, int(inventory.InventoryPage.TotalItems))
			assertItemIDsOnFinalPageOnly(t, tc.Calls, tt.args.wantTotalItems)
		})
	}
}

func TestManageInfiniBandPartition_CreateInfiniBandPartitionOnSite(t *testing.T) {
	coreClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreClient.SwapClient(cClient.NewMockCoreGrpcClient())
	manager := NewManageInfiniBandPartition(coreClient)

	id := &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"}
	config := &corev1.IBPartitionConfig{TenantOrganizationId: "best_org_ever"}
	metadata := &corev1.Metadata{Name: "best_ibpartition_ever"}

	// Older workflow payloads carry the name only in config.
	var legacyRequest corev1.IBPartitionCreationRequest
	err := protojson.Unmarshal([]byte(`{
		"id": {"value": "b410867c-655a-11ef-bc4a-0393098e5d09"},
		"config": {"name": "legacy-partition", "tenantOrganizationId": "best_org_ever"}
	}`), &legacyRequest)
	require.NoError(t, err)

	tests := []struct {
		name    string
		request *corev1.IBPartitionCreationRequest
		wantErr string
	}{
		{
			name:    "metadata name",
			request: &corev1.IBPartitionCreationRequest{Id: id, Config: config, Metadata: metadata},
		},
		{
			name:    "legacy config name",
			request: &legacyRequest,
		},
		{
			name:    "missing name",
			request: &corev1.IBPartitionCreationRequest{Id: id, Config: config},
			wantErr: "without Name",
		},
		{
			name:    "missing organization",
			request: &corev1.IBPartitionCreationRequest{Id: id, Config: &corev1.IBPartitionConfig{}, Metadata: metadata},
			wantErr: "without TenantOrganizationId",
		},
		{
			name:    "missing config",
			request: &corev1.IBPartitionCreationRequest{Id: id, Metadata: metadata},
			wantErr: "without Config",
		},
		{
			name:    "missing ID",
			request: &corev1.IBPartitionCreationRequest{Config: config, Metadata: metadata},
			wantErr: "without ID",
		},
		{
			name:    "missing request",
			wantErr: "empty create InfiniBand Partition request",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := manager.CreateInfiniBandPartitionOnSite(context.Background(), tt.request)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestManageIBPartition_DeleteIBPartitionOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.IBPartitionDeletionRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete ibpartition success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.IBPartitionDeletionRequest{
					Id: &corev1.IBPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
				},
			},
			wantErr: false,
		},
		{
			name: "test delete ibpartition fail on blank id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.IBPartitionDeletionRequest{
					Id: &corev1.IBPartitionId{Value: ""},
				},
			},
			wantErr: true,
		},
		{
			name: "test delete ibpartition fail on missing request",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: nil,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageInfiniBandPartition(tt.fields.coreGrpcAtomicClient)
			err := mm.DeleteInfiniBandPartitionOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
