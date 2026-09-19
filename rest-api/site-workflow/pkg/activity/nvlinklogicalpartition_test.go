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

func TestManageNVLinkLogicalPartitionInventory_DiscoverNVLinkLogicalPartitionInventory(t *testing.T) {
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
		findIDsError   error
	}
	tests := []struct {
		name   string
		fields fields
		args   args
	}{
		{
			name: "test collecting and publishing nvlink logical partition inventory, empty inventory",
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
			name: "test collecting and publishing nvlink logical partition inventory, normal inventory",
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

			manageInstance := NewManageNVLinkLogicalPartitionInventory(ManageInventoryConfig{
				SiteID:                tt.fields.siteID,
				CoreGrpcAtomicClient:  tt.fields.coreGrpcAtomicClient,
				TemporalPublishClient: tc,
				TemporalPublishQueue:  tt.fields.temporalPublishQueue,
				SitePageSize:          tt.fields.sitePageSize,
				CloudPageSize:         tt.fields.cloudPageSize,
			})

			ctx := context.Background()
			ctx = context.WithValue(ctx, "wantCount", tt.args.wantTotalItems)
			if tt.args.findIDsError != nil {
				ctx = context.WithValue(ctx, "wantError", tt.args.findIDsError)
			}

			totalPages := tt.args.wantTotalItems / tt.fields.cloudPageSize
			if tt.args.wantTotalItems%tt.fields.cloudPageSize > 0 {
				totalPages++
			}

			err := manageInstance.DiscoverNVLinkLogicalPartitionInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.NVLinkLogicalPartitionInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.Partitions))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.Partitions))
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

func TestManageNVLinkLogicalPartition_CreateNVLinkLogicalPartitionOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.NVLinkLogicalPartitionCreationRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create nvlink logical partition success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionCreationRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "test_name",
						},
						TenantOrganizationId: "test_org",
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test create nvlink logical partition fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionCreationRequest{
					Id: nil,
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "test_name",
						},
						TenantOrganizationId: "test_org",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create nvlink logical partition fail on missing name",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionCreationRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "",
						},
						TenantOrganizationId: "test_org",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create nvlink logical partition fail on missing org",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionCreationRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "test_name",
						},
						TenantOrganizationId: "",
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create nvlink logical partition fail on missing request",
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
			mnvllp := NewManageNVLinkLogicalPartition(tt.fields.coreGrpcAtomicClient)
			nvLinkLogicalPartition, err := mnvllp.CreateNVLinkLogicalPartitionOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
				if tt.args.request != nil {
					assert.NotNil(t, nvLinkLogicalPartition)
					assert.Equal(t, tt.args.request.Id.Value, nvLinkLogicalPartition.Id.Value)
					assert.Equal(t, tt.args.request.Config.Metadata.Name, nvLinkLogicalPartition.Config.Metadata.Name)
					assert.Equal(t, tt.args.request.Config.TenantOrganizationId, nvLinkLogicalPartition.Config.TenantOrganizationId)
				}
			}
		})
	}
}

func TestManageNVLinkLogicalPartition_UpdateNVLinkLogicalPartitionOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.NVLinkLogicalPartitionUpdateRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update nvlink logical partition success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionUpdateRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "test_name",
						},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test update nvlink logical partition fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionUpdateRequest{
					Id: nil,
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "test_name",
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create nvlink logical partition fail on missing name",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionUpdateRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
					Config: &corev1.NVLinkLogicalPartitionConfig{
						Metadata: &corev1.Metadata{
							Name: "",
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test update nvlink logical partition fail on missing request",
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
			mnvllp := NewManageNVLinkLogicalPartition(tt.fields.coreGrpcAtomicClient)
			err := mnvllp.UpdateNVLinkLogicalPartitionOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageNVLinkLogicalPartition_DeleteNVLinkLogicalPartitionOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.NVLinkLogicalPartitionDeletionRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete nvlink logical partition success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionDeletionRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: "b410867c-655a-11ef-bc4a-0393098e5d09"},
				},
			},
			wantErr: false,
		},
		{
			name: "test delete nvlink logical partition fail on blank id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.NVLinkLogicalPartitionDeletionRequest{
					Id: &corev1.NVLinkLogicalPartitionId{Value: ""},
				},
			},
			wantErr: true,
		},
		{
			name: "test delete nvlink logical partition fail on missing request",
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
			mm := NewManageNVLinkLogicalPartition(tt.fields.coreGrpcAtomicClient)
			err := mm.DeleteNVLinkLogicalPartitionOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
