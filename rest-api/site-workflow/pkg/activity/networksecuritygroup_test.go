// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	"github.com/stretchr/testify/mock"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

func TestManageNetworkSecurityGroup_UpdateNetworkSecurityGroupOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	labelKey := "key1"
	labelValue := "value1"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.UpdateNetworkSecurityGroupRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test NetworkSecurityGroup update success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateNetworkSecurityGroupRequest{
					Id:                   uuid.NewString(),
					TenantOrganizationId: "anything",
					Metadata: &corev1.Metadata{
						Name:        "updated_name",
						Description: "updated_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test NetworkSecurityGroup update missing id in request fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateNetworkSecurityGroupRequest{
					Id:                   "",
					TenantOrganizationId: "anything",
					Metadata: &corev1.Metadata{
						Name:        "updated_name",
						Description: "updated_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test NetworkSecurityGroup update missing tenant id in request fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateNetworkSecurityGroupRequest{
					Id:                   "anything",
					TenantOrganizationId: "",
					Metadata: &corev1.Metadata{
						Name:        "updated_name",
						Description: "updated_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test NetworkSecurityGroup update nil request fail",
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
			mm := NewManageNetworkSecurityGroup(tt.fields.coreGrpcAtomicClient)
			err := mm.UpdateNetworkSecurityGroupOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageNetworkSecurityGroup_CreateNetworkSecurityGroupOnSiteOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	labelKey := "key1"
	labelValue := "value1"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.CreateNetworkSecurityGroupRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create NetworkSecurityGroup success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateNetworkSecurityGroupRequest{
					Id:                   util.GetStrPtr(uuid.NewString()),
					TenantOrganizationId: "anything",
					Metadata: &corev1.Metadata{
						Name:        "new_name",
						Description: "new_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: false,
		},

		{
			name: "test create NetworkSecurityGroup nil request fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: nil,
			},
			wantErr: true,
		},

		{
			name: "test create NetworkSecurityGroup missing id in request fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateNetworkSecurityGroupRequest{
					Id:                   util.GetStrPtr(""),
					TenantOrganizationId: "anything",
					Metadata: &corev1.Metadata{
						Name:        "new_name",
						Description: "new_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: true,
		},

		{
			name: "test create NetworkSecurityGroup missing tenant id in request fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateNetworkSecurityGroupRequest{
					Id: util.GetStrPtr("anything"),
					Metadata: &corev1.Metadata{
						Name:        "new_name",
						Description: "new_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: true,
		},

		{
			name: "test create NetworkSecurityGroup nil id in request fail",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateNetworkSecurityGroupRequest{
					Id:                   nil,
					TenantOrganizationId: "anything",
					Metadata: &corev1.Metadata{
						Name:        "new_name",
						Description: "new_description",
						Labels: []*corev1.Label{
							{
								Key:   labelKey,
								Value: &labelValue,
							},
						},
					},
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageNetworkSecurityGroup(tt.fields.coreGrpcAtomicClient)
			err := mm.CreateNetworkSecurityGroupOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageNetworkSecurityGroupInventory_DiscoverNetworkSecurityGroupInventory(t *testing.T) {
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
			name: "test collecting and publishing networkSecurityGroup inventory, empty inventory",
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
			name: "test collecting and publishing networkSecurityGroup inventory, normal inventory",
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

			manageNetworkSecurityGroup := NewManageNetworkSecurityGroupInventory(ManageInventoryConfig{
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

			err := manageNetworkSecurityGroup.DiscoverNetworkSecurityGroupInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.NetworkSecurityGroupInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.NetworkSecurityGroups))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.NetworkSecurityGroups))
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

func TestManageNetworkSecurityGroup_DeleteNetworkSecurityGroupOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.DeleteNetworkSecurityGroupRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete NetworkSecurityGroup success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteNetworkSecurityGroupRequest{
					Id:                   uuid.NewString(),
					TenantOrganizationId: "anything",
				},
			},
			wantErr: false,
		},
		{
			name: "test delete NetworkSecurityGroup with nil ID failure",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteNetworkSecurityGroupRequest{
					Id:                   "",
					TenantOrganizationId: "anything",
				},
			},
			wantErr: true,
		},
		{
			name: "test delete NetworkSecurityGroup with missing tenant ID failure",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteNetworkSecurityGroupRequest{
					Id:                   "anything",
					TenantOrganizationId: "",
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageNetworkSecurityGroup(tt.fields.coreGrpcAtomicClient)
			err := mm.DeleteNetworkSecurityGroupOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
