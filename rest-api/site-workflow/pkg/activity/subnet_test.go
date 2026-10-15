// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/stretchr/testify/assert"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestManageSubnet_CreateSubnetOnSiteOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	subnetName := "the_best_subnet"
	vpcID := "9001"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.NetworkSegmentCreationRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create Subnet success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentCreationRequest{
					Name:  subnetName,
					VpcId: &corev1.VpcId{Value: vpcID},
					Prefixes: []*corev1.NetworkPrefix{
						&corev1.NetworkPrefix{
							Prefix: "10.0.0.1/8",
						},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test create Subnet, bad prefix, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentCreationRequest{
					Name:  subnetName,
					VpcId: &corev1.VpcId{Value: vpcID},
					Prefixes: []*corev1.NetworkPrefix{
						&corev1.NetworkPrefix{
							Prefix: "abc123",
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create Subnet, empty prefix list, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentCreationRequest{
					Name:     subnetName,
					VpcId:    &corev1.VpcId{Value: vpcID},
					Prefixes: nil,
				},
			},
			wantErr: true,
		},
		{
			name: "test create Subnet, nil prefix in list, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentCreationRequest{
					Name:  subnetName,
					VpcId: &corev1.VpcId{Value: vpcID},
					Prefixes: []*corev1.NetworkPrefix{
						nil,
						&corev1.NetworkPrefix{
							Prefix: "10.0.0.1/8",
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create Subnet, no name, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentCreationRequest{
					VpcId: &corev1.VpcId{Value: vpcID},
					Prefixes: []*corev1.NetworkPrefix{
						&corev1.NetworkPrefix{
							Prefix: "10.0.0.1/8",
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create Subnet, no vpc ID, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentCreationRequest{
					Name: subnetName,
					Prefixes: []*corev1.NetworkPrefix{
						&corev1.NetworkPrefix{
							Prefix: "10.0.0.1/8",
						},
					},
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageSubnet(tt.fields.coreGrpcAtomicClient)
			err := mm.CreateSubnetOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageSubnet_DeleteSubnetOnSiteOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	subnetID := "555"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.NetworkSegmentDeletionRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete Subnet success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentDeletionRequest{
					Id: &corev1.NetworkSegmentId{Value: subnetID},
				},
			},
			wantErr: false,
		},
		{
			name: "test delete Subnet, missing ID, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentDeletionRequest{
					Id: nil,
				},
			},
			wantErr: true,
		},
		{
			name: "test delete Subnet, empty ID, fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NetworkSegmentDeletionRequest{
					Id: &corev1.NetworkSegmentId{Value: ""},
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageSubnet(tt.fields.coreGrpcAtomicClient)
			err := mm.DeleteSubnetOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageSubnetInventory_DiscoverSubnetInventory(t *testing.T) {
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
			name: "test collecting and publishing subnet inventory, empty inventory",
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
			name: "test collecting and publishing subnet inventory, normal inventory",
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

			manageSubnetInventory := NewManageSubnetInventory(ManageInventoryConfig{
				SiteID:                tt.fields.siteID,
				CoreGrpcAtomicClient:  tt.fields.coreGrpcAtomicClient,
				TemporalPublishClient: tc,
				TemporalPublishQueue:  tt.fields.temporalPublishQueue,
				SitePageSize:          tt.fields.sitePageSize,
				CloudPageSize:         tt.fields.cloudPageSize,
			})

			ctx := context.Background()
			ctx = context.WithValue(ctx, "wantCount", tt.args.wantTotalItems)

			totalPages := tt.args.wantTotalItems / tt.fields.cloudPageSize
			if tt.args.wantTotalItems%tt.fields.cloudPageSize > 0 {
				totalPages++
			}

			err := manageSubnetInventory.DiscoverSubnetInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.SubnetInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.Segments))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.Segments))
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
