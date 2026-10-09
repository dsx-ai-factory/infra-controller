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
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestManageExpectedMachineInventory_DiscoverExpectedMachineInventory(t *testing.T) {
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
			name: "test collecting and publishing expected machine inventory, empty inventory",
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
			name: "test collecting and publishing expected machine inventory, normal inventory",
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
		{
			name: "test collecting and publishing expected machine inventory fallback, empty inventory",
			fields: fields{
				siteID:               uuid.New(),
				coreGrpcAtomicClient: coreGrpcAtomicClient,
				temporalPublishQueue: "test-queue",
				sitePageSize:         100,
				cloudPageSize:        25,
			},
			args: args{
				wantTotalItems: 0,
				findIDsError:   status.Error(codes.Unimplemented, "not implemented"),
			},
		},
		{
			name: "test collecting and publishing expected machine inventory fallback, normal inventory",
			fields: fields{
				siteID:               uuid.New(),
				coreGrpcAtomicClient: coreGrpcAtomicClient,
				temporalPublishQueue: "test-queue",
				sitePageSize:         100,
				cloudPageSize:        25,
			},
			args: args{
				wantTotalItems: 195,
				findIDsError:   status.Error(codes.Unimplemented, "not implemented"),
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tc := &tmocks.Client{}
			tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
				mock.AnythingOfType("string"), mock.AnythingOfType("uuid.UUID"), mock.Anything).Return(wrun, nil)
			tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)

			manageInstance := NewManageExpectedMachineInventory(
				tt.fields.siteID,
				tt.fields.coreGrpcAtomicClient,
				tc,
				tt.fields.temporalPublishQueue,
				tt.fields.cloudPageSize,
			)

			ctx := context.Background()
			ctx = context.WithValue(ctx, "wantCount", tt.args.wantTotalItems)
			if tt.args.findIDsError != nil {
				ctx = context.WithValue(ctx, "wantError", tt.args.findIDsError)
			}

			totalPages := tt.args.wantTotalItems / tt.fields.cloudPageSize
			if tt.args.wantTotalItems%tt.fields.cloudPageSize > 0 {
				totalPages++
			}

			err := manageInstance.DiscoverExpectedMachineInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.ExpectedMachineInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.ExpectedMachines))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.ExpectedMachines))
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

func TestManageExpectedMachine_CreateExpectedMachineOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedMachine
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create expected machine success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-machine-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN123456789",
				},
			},
			wantErr: false,
		},
		{
			name: "test create expected machine fail on missing MAC address",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-machine-002"},
					BmcMacAddress:       "",
					ChassisSerialNumber: "SN123456789",
				},
			},
			wantErr: true, // This should fail since MAC address is missing (now required)
		},
		{
			name: "test create expected machine fail on missing serial number",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-machine-003"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "",
				},
			},
			wantErr: true, // This should fail since serial number is missing (now required)
		},
		{
			name: "test create expected machine fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  nil,
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN123456789",
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected machine fail on missing identifying information",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-machine-004"},
					BmcMacAddress:       "",
					ChassisSerialNumber: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected machine fail on missing request",
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
			mm := NewManageExpectedMachine(tt.fields.coreGrpcAtomicClient)
			err := mm.CreateExpectedMachineOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedMachine_UpdateExpectedMachineOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedMachine
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update expected machine success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-update-001"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN123456789",
				},
			},
			wantErr: false,
		},
		{
			name: "test update expected machine fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  nil,
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "SN123456789",
				},
			},
			wantErr: true,
		},
		{
			name: "test update expected machine fail on missing MAC address",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-update-002"},
					BmcMacAddress:       "",
					ChassisSerialNumber: "SN123456789",
				},
			},
			wantErr: true, // This should fail since MAC address is missing (now required)
		},
		{
			name: "test update expected machine fail on missing serial number",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-update-003"},
					BmcMacAddress:       "00:11:22:33:44:55",
					ChassisSerialNumber: "",
				},
			},
			wantErr: true, // This should fail since serial number is missing (now required)
		},
		{
			name: "test update expected machine fail on missing both MAC and serial",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachine{
					Id:                  &corev1.UUID{Value: "test-update-004"},
					BmcMacAddress:       "",
					ChassisSerialNumber: "",
				},
			},
			wantErr: true, // This should fail since both MAC address and serial number are missing
		},
		{
			name: "test update expected machine fail on missing request",
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
			mm := NewManageExpectedMachine(tt.fields.coreGrpcAtomicClient)
			err := mm.UpdateExpectedMachineOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedMachine_DeleteExpectedMachineOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedMachineRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete expected machine success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachineRequest{
					Id:            &corev1.UUID{Value: "test-delete-001"},
					BmcMacAddress: "00:11:22:33:44:55",
				},
			},
			wantErr: false,
		},
		{
			name: "test delete expected machine fail on missing id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachineRequest{
					Id:            nil,
					BmcMacAddress: "00:11:22:33:44:55",
				},
			},
			wantErr: true,
		},
		{
			name: "test delete expected machine success with missing BMC MAC address",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedMachineRequest{
					Id:            &corev1.UUID{Value: "test-delete-002"},
					BmcMacAddress: "",
				},
			},
			wantErr: false,
		},
		{
			name: "test delete expected machine fail on missing request",
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
			mm := NewManageExpectedMachine(tt.fields.coreGrpcAtomicClient)
			err := mm.DeleteExpectedMachineOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedMachine_CreateExpectedMachinesOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.BatchExpectedMachineOperationRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create expected machines success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.BatchExpectedMachineOperationRequest{
					ExpectedMachines: &corev1.ExpectedMachineList{
						ExpectedMachines: []*corev1.ExpectedMachine{
							{
								Id:                  &corev1.UUID{Value: "test-batch-001"},
								BmcMacAddress:       "00:11:22:33:44:55",
								ChassisSerialNumber: "SN123456789",
							},
							{
								Id:                  &corev1.UUID{Value: "test-batch-002"},
								BmcMacAddress:       "00:11:22:33:44:66",
								ChassisSerialNumber: "SN987654321",
							},
						},
					},
					AcceptPartialResults: true,
				},
			},
			wantErr: false,
		},
		{
			name: "test create expected machines fail on empty list",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.BatchExpectedMachineOperationRequest{
					ExpectedMachines: &corev1.ExpectedMachineList{
						ExpectedMachines: []*corev1.ExpectedMachine{},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test create expected machines fail on nil request",
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
			mm := NewManageExpectedMachine(tt.fields.coreGrpcAtomicClient)
			response, err := mm.CreateExpectedMachinesOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, response)
				assert.Equal(t, len(tt.args.request.ExpectedMachines.ExpectedMachines), len(response.Results), "Should have result for each machine")

				// Verify that each result includes ID and MAC address via ExpectedMachine payload
				for i, result := range response.Results {
					assert.NotNil(t, result.GetExpectedMachine())
					assert.NotNil(t, result.GetExpectedMachine().GetId())
					assert.Equal(t, tt.args.request.ExpectedMachines.ExpectedMachines[i].GetId().GetValue(), result.GetExpectedMachine().GetId().GetValue(), "ID should be included in result")
					assert.Equal(t, tt.args.request.ExpectedMachines.ExpectedMachines[i].BmcMacAddress, result.GetExpectedMachine().GetBmcMacAddress(), "MAC address should be included in result")
				}
			}
		})
	}
}

func TestManageExpectedMachine_UpdateExpectedMachinesOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.BatchExpectedMachineOperationRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update expected machines success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.BatchExpectedMachineOperationRequest{
					ExpectedMachines: &corev1.ExpectedMachineList{
						ExpectedMachines: []*corev1.ExpectedMachine{
							{
								Id:                  &corev1.UUID{Value: "test-batch-update-001"},
								BmcMacAddress:       "00:11:22:33:44:55",
								ChassisSerialNumber: "SN123456789",
							},
							{
								Id:                  &corev1.UUID{Value: "test-batch-update-002"},
								BmcMacAddress:       "00:11:22:33:44:66",
								ChassisSerialNumber: "SN987654321",
							},
						},
					},
					AcceptPartialResults: true,
				},
			},
			wantErr: false,
		},
		{
			name: "test update expected machines fail on empty list",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.BatchExpectedMachineOperationRequest{
					ExpectedMachines: &corev1.ExpectedMachineList{
						ExpectedMachines: []*corev1.ExpectedMachine{},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test update expected machines fail on nil request",
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
			mm := NewManageExpectedMachine(tt.fields.coreGrpcAtomicClient)
			response, err := mm.UpdateExpectedMachinesOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Nil(t, response)
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, response)
				assert.Equal(t, len(tt.args.request.ExpectedMachines.ExpectedMachines), len(response.Results), "Should have result for each machine")

				// Verify that each result includes ID and MAC address via ExpectedMachine payload
				for i, result := range response.Results {
					assert.NotNil(t, result.GetExpectedMachine())
					assert.NotNil(t, result.GetExpectedMachine().GetId())
					assert.Equal(t, tt.args.request.ExpectedMachines.ExpectedMachines[i].GetId().GetValue(), result.GetExpectedMachine().GetId().GetValue(), "ID should be included in result")
					assert.Equal(t, tt.args.request.ExpectedMachines.ExpectedMachines[i].BmcMacAddress, result.GetExpectedMachine().GetBmcMacAddress(), "MAC address should be included in result")
				}
			}
		})
	}
}

func TestManageExpectedMachine_CreateExpectedMachineOnFlow(t *testing.T) {
	manager := NewManageExpectedMachine(nil)

	assert.NoError(t, manager.CreateExpectedMachineOnFlow(context.Background(), nil))
	assert.NoError(t, manager.CreateExpectedMachineOnFlow(context.Background(), &corev1.ExpectedMachine{}))
}

func TestManageExpectedMachine_CreateExpectedMachinesOnFlow(t *testing.T) {
	manager := NewManageExpectedMachine(nil)

	assert.NoError(t, manager.CreateExpectedMachinesOnFlow(context.Background(), nil))
	assert.NoError(t, manager.CreateExpectedMachinesOnFlow(context.Background(), &corev1.BatchExpectedMachineOperationRequest{}))
}
