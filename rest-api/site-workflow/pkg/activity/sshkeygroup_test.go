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
	tmocks "go.temporal.io/sdk/mocks"
)

func TestManageSSHKeyGroupInventory_DiscoverSSHKeyGroupInventory(t *testing.T) {
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
			name: "test collecting and publishing ssh key group inventory, empty inventory",
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
			name: "test collecting and publishing ssh key group inventory, normal inventory",
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

			manageInstance := NewManageSSHKeyGroupInventory(ManageInventoryConfig{
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

			err := manageInstance.DiscoverSSHKeyGroupInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.SSHKeyGroupInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.TenantKeysets))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.TenantKeysets))
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

func TestManageSSHKeyGroup_CreateSSHKeyGroupOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	orgID := "m4jjok8wsg"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.CreateTenantKeysetRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create SSH Key Group success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
					Version: "1209381123445",
				},
			},
			wantErr: false,
		},
		{
			name: "test create SSH Key Group fails on missing org ID",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: "",
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
					Version: "1209381123445",
				},
			},
			wantErr: true,
		},
		{
			name: "test create SSH Key Group fails on missing source keyset id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "",
					},
					Version: "1209381123445",
				},
			},
			wantErr: true,
		},
		{
			name: "test create SSH Key Group fails on missing version",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
					Version: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test create SSH Key Group fails on missing request",
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
			mt := NewManageSSHKeyGroup(tt.fields.coreGrpcAtomicClient)
			err := mt.CreateSSHKeyGroupOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageSSHKeyGroup_UpdateSSHKeyGroupOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	orgID := "m4jjok8wsg"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.UpdateTenantKeysetRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update SSH Key Group success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
					Version: "1209381123445",
				},
			},
			wantErr: false,
		},
		{
			name: "test update SSH Key Group fails on missing org ID",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: "",
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
					Version: "1209381123445",
				},
			},
			wantErr: true,
		},
		{
			name: "test update SSH Key Group fails on missing keyset id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "",
					},
					Version: "1209381123445",
				},
			},
			wantErr: true,
		},
		{
			name: "test update SSH Key Group fails on missing version",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
					Version: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test update SSH Key Group fails on missing request",
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
			mt := NewManageSSHKeyGroup(tt.fields.coreGrpcAtomicClient)
			err := mt.UpdateSSHKeyGroupOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageSSHKeyGroup_DeleteSSHKeyGroupOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	orgID := "m4jjok8wsg"

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.DeleteTenantKeysetRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete SSH Key Group success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test delete SSH Key Group fails on missing org ID",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: "",
						KeysetId:       "547f73dc-16e6-4cc0-a0c1-3911dbc2aec3",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test delete SSH Key Group fails on missing keyset ID",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						OrganizationId: orgID,
						KeysetId:       "",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test delete SSH Key Group fails on missing request",
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
			mt := NewManageSSHKeyGroup(tt.fields.coreGrpcAtomicClient)
			err := mt.DeleteSSHKeyGroupOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
