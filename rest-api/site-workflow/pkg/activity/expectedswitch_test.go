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

func TestManageExpectedSwitchInventory_DiscoverExpectedSwitchInventory(t *testing.T) {
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
			name: "test collecting and publishing expected switch inventory, empty inventory",
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
			name: "test collecting and publishing expected switch inventory, normal inventory",
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

			manageInstance := NewManageExpectedSwitchInventory(
				tt.fields.siteID,
				tt.fields.coreGrpcAtomicClient,
				tc,
				tt.fields.temporalPublishQueue,
				tt.fields.cloudPageSize,
			)

			ctx := context.Background()
			ctx = context.WithValue(ctx, "wantCount", tt.args.wantTotalItems)

			totalPages := tt.args.wantTotalItems / tt.fields.cloudPageSize
			if tt.args.wantTotalItems%tt.fields.cloudPageSize > 0 {
				totalPages++
			}

			err := manageInstance.DiscoverExpectedSwitchInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.ExpectedSwitchInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.ExpectedSwitches))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.ExpectedSwitches))
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

func TestManageExpectedSwitch_CreateExpectedSwitchOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedSwitch
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create expected switch success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-switch-001"},
					BmcMacAddress:      "00:11:22:33:44:55",
					SwitchSerialNumber: "SWITCH-123456789",
				},
			},
			wantErr: false,
		},
		{
			name: "test create expected switch fail on missing MAC address",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-switch-002"},
					BmcMacAddress:      "",
					SwitchSerialNumber: "SWITCH-123456789",
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected switch fail on missing serial number",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-switch-003"},
					BmcMacAddress:      "00:11:22:33:44:55",
					SwitchSerialNumber: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected switch fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   nil,
					BmcMacAddress:      "00:11:22:33:44:55",
					SwitchSerialNumber: "SWITCH-123456789",
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected switch fail on missing identifying information",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-switch-004"},
					BmcMacAddress:      "",
					SwitchSerialNumber: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected switch fail on missing request",
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
			mm := NewManageExpectedSwitch(tt.fields.coreGrpcAtomicClient)
			err := mm.CreateExpectedSwitchOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedSwitch_UpdateExpectedSwitchOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedSwitch
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update expected switch success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-update-001"},
					BmcMacAddress:      "00:11:22:33:44:55",
					SwitchSerialNumber: "SWITCH-123456789",
				},
			},
			wantErr: false,
		},
		{
			name: "test update expected switch fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   nil,
					BmcMacAddress:      "00:11:22:33:44:55",
					SwitchSerialNumber: "SWITCH-123456789",
				},
			},
			wantErr: true,
		},
		{
			name: "test update expected switch fail on missing MAC address",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-update-002"},
					BmcMacAddress:      "",
					SwitchSerialNumber: "SWITCH-123456789",
				},
			},
			wantErr: true,
		},
		{
			name: "test update expected switch fail on missing serial number",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-update-003"},
					BmcMacAddress:      "00:11:22:33:44:55",
					SwitchSerialNumber: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test update expected switch fail on missing both MAC and serial",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitch{
					ExpectedSwitchId:   &corev1.UUID{Value: "test-update-004"},
					BmcMacAddress:      "",
					SwitchSerialNumber: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test update expected switch fail on missing request",
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
			mm := NewManageExpectedSwitch(tt.fields.coreGrpcAtomicClient)
			err := mm.UpdateExpectedSwitchOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedSwitch_DeleteExpectedSwitchOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedSwitchRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete expected switch success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitchRequest{
					ExpectedSwitchId: &corev1.UUID{Value: "test-delete-001"},
					BmcMacAddress:    "00:11:22:33:44:55",
				},
			},
			wantErr: false,
		},
		{
			name: "test delete expected switch fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitchRequest{
					ExpectedSwitchId: nil,
					BmcMacAddress:    "00:11:22:33:44:55",
				},
			},
			wantErr: true,
		},
		{
			name: "test delete expected switch success with missing BMC MAC address",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedSwitchRequest{
					ExpectedSwitchId: &corev1.UUID{Value: "test-delete-002"},
					BmcMacAddress:    "",
				},
			},
			wantErr: false,
		},
		{
			name: "test delete expected switch fail on missing request",
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
			mm := NewManageExpectedSwitch(tt.fields.coreGrpcAtomicClient)
			err := mm.DeleteExpectedSwitchOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedSwitch_CreateExpectedSwitchOnFlow(t *testing.T) {
	manager := NewManageExpectedSwitch(nil)

	assert.NoError(t, manager.CreateExpectedSwitchOnFlow(context.Background(), nil))
	assert.NoError(t, manager.CreateExpectedSwitchOnFlow(context.Background(), &corev1.ExpectedSwitch{}))
}
