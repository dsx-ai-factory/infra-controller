// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
)

func TestManageDpuExtensionService_CreateDpuExtensionServiceOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		CoreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.CreateDpuExtensionServiceRequest
	}

	serviceID := "test-service-id"
	serviceName := "test-service-name"
	tenantOrgID := "test-tenant-org-id"

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create DpuExtensionService success",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateDpuExtensionServiceRequest{
					ServiceId:            &serviceID,
					ServiceName:          serviceName,
					TenantOrganizationId: tenantOrgID,
				},
			},
			wantErr: false,
		},
		{
			name: "test create DpuExtensionService fail on missing ServiceId",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateDpuExtensionServiceRequest{
					ServiceId:            nil,
					ServiceName:          serviceName,
					TenantOrganizationId: tenantOrgID,
				},
			},
			wantErr: true,
		},
		{
			name: "test create DpuExtensionService fail on empty ServiceId",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateDpuExtensionServiceRequest{
					ServiceId:            util.GetStrPtr(""),
					ServiceName:          serviceName,
					TenantOrganizationId: tenantOrgID,
				},
			},
			wantErr: true,
		},
		{
			name: "test create DpuExtensionService fail on missing ServiceName",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateDpuExtensionServiceRequest{
					ServiceId:            &serviceID,
					ServiceName:          "",
					TenantOrganizationId: tenantOrgID,
				},
			},
			wantErr: true,
		},
		{
			name: "test create DpuExtensionService fail on missing TenantOrganizationId",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.CreateDpuExtensionServiceRequest{
					ServiceId:            &serviceID,
					ServiceName:          serviceName,
					TenantOrganizationId: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test create DpuExtensionService fail on missing request",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
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
			mm := NewManageDpuExtensionService(tt.fields.CoreGrpcAtomicClient)
			result, err := mm.CreateDpuExtensionServiceOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			require.NoError(t, err)

			if tt.args.request.ServiceId != nil {
				assert.Equal(t, *tt.args.request.ServiceId, result.ServiceId)
			}
			assert.Equal(t, tt.args.request.ServiceName, result.ServiceName)
			assert.Equal(t, tt.args.request.TenantOrganizationId, result.TenantOrganizationId)
		})
	}
}

func TestManageDpuExtensionService_UpdateDpuExtensionServiceOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		CoreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.UpdateDpuExtensionServiceRequest
	}

	serviceID := "test-service-id"

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update DpuExtensionService success",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateDpuExtensionServiceRequest{
					ServiceId:   serviceID,
					ServiceName: util.GetStrPtr("test-service-name"),
				},
			},
			wantErr: false,
		},
		{
			name: "test update DpuExtensionService fail on missing ServiceId",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.UpdateDpuExtensionServiceRequest{
					ServiceId: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test update DpuExtensionService fail on missing request",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
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
			mm := NewManageDpuExtensionService(tt.fields.CoreGrpcAtomicClient)
			result, err := mm.UpdateDpuExtensionServiceOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			require.NoError(t, err)

			assert.Equal(t, tt.args.request.ServiceId, result.ServiceId)

			if tt.args.request.ServiceName != nil {
				assert.Equal(t, *tt.args.request.ServiceName, result.ServiceName)
			}
		})
	}
}

func TestManageDpuExtensionService_DeleteDpuExtensionServiceOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		CoreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.DeleteDpuExtensionServiceRequest
	}

	serviceID := "test-service-id"

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete DpuExtensionService success",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteDpuExtensionServiceRequest{
					ServiceId: serviceID,
				},
			},
			wantErr: false,
		},
		{
			name: "test delete DpuExtensionService fail on missing ServiceId",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.DeleteDpuExtensionServiceRequest{
					ServiceId: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test delete DpuExtensionService fail on missing request",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
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
			mm := NewManageDpuExtensionService(tt.fields.CoreGrpcAtomicClient)
			err := mm.DeleteDpuExtensionServiceOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageDpuExtensionService_GetDpuExtensionServiceVersionsInfoOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		CoreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.GetDpuExtensionServiceVersionsInfoRequest
	}

	serviceID := "test-service-id"

	tests := []struct {
		name      string
		fields    fields
		args      args
		wantCount int
		wantErr   bool
	}{
		{
			name: "test get DpuExtensionService versions info success",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.WithValue(context.Background(), "wantCount", 20),
				request: &corev1.GetDpuExtensionServiceVersionsInfoRequest{
					ServiceId: serviceID,
				},
			},
			wantCount: 20,
			wantErr:   false,
		},
		{
			name: "test get DpuExtensionService versions info fail on missing ServiceId",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.GetDpuExtensionServiceVersionsInfoRequest{
					ServiceId: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test get DpuExtensionService versions info fail on missing request",
			fields: fields{
				CoreGrpcAtomicClient: coreGrpcAtomicClient,
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
			mm := NewManageDpuExtensionService(tt.fields.CoreGrpcAtomicClient)
			versionInfoList, err := mm.GetDpuExtensionServiceVersionsInfoOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantCount, len(versionInfoList.VersionInfos))
			}
		})
	}
}

func TestManageDpuExtensionServiceInventory_DiscoverDpuExtensionServiceInventory(t *testing.T) {
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
			name: "test collecting and publishing dpu extension service inventory, empty inventory",
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
			name: "test collecting and publishing dpu extension service inventory, normal inventory",
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

			manageDpuExtensionService := NewManageDpuExtensionServiceInventory(ManageInventoryConfig{
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

			err := manageDpuExtensionService.DiscoverDpuExtensionServiceInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.DpuExtensionServiceInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.DpuExtensionServices))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.DpuExtensionServices))
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
