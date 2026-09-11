// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/proto"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
)

func TestManageMachine_SetMachineMaintenanceOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.MaintenanceRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test enabling Machine maintenance mode success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.MaintenanceRequest{
					Operation: corev1.MaintenanceOperation_Enable,
					HostId:    &corev1.MachineId{Id: "test-machine-id"},
					Reference: util.GetStrPtr("test-reference"),
				},
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageMachine(tt.fields.coreGrpcAtomicClient)
			err := mm.SetMachineMaintenanceOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageMachine_UpdateMachineMetadataOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.MachineMetadataUpdateRequest
	}

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test updating Machine metadata success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.MachineMetadataUpdateRequest{
					MachineId: &corev1.MachineId{Id: "test-machine-id"},
					Metadata: &corev1.Metadata{
						Labels: []*corev1.Label{
							{
								Key:   "test-key",
								Value: util.GetStrPtr("test-value"),
							},
						},
					},
				},
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := NewManageMachine(tt.fields.coreGrpcAtomicClient)
			err := mm.UpdateMachineMetadataOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageMachine_CreateMachineHealthReportOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	mm := NewManageMachine(coreGrpcAtomicClient)
	req := &corev1.InsertMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: "machine-1"},
		HealthReportEntry: &corev1.HealthReportEntry{
			Report: &corev1.HealthReport{
				Source: "request-online-repair",
				Alerts: []*corev1.HealthProbeAlert{
					{Id: "OnLineRepair", Message: `{"details":"d","issue_category":"OTHER","summary":"s"}`},
				},
			},
			Mode: corev1.HealthReportApplyMode_Merge,
		},
	}
	assert.NoError(t, mm.CreateMachineHealthReportOnSite(context.Background(), req))

	err := mm.CreateMachineHealthReportOnSite(context.Background(), nil)
	assert.Error(t, err)
}

func TestManageMachine_DeleteMachineHealthReportOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	mm := NewManageMachine(coreGrpcAtomicClient)
	req := &corev1.RemoveMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: "machine-1"},
		Source:    "request-online-repair",
	}
	assert.NoError(t, mm.DeleteMachineHealthReportOnSite(context.Background(), req))

	err := mm.DeleteMachineHealthReportOnSite(context.Background(), nil)
	assert.Error(t, err)
}

//nolint:staticcheck // Asserting the deprecated fields were cleared means reading them.
func Test_pruneMachineForPublish(t *testing.T) {
	events := func(versions ...string) []*corev1.MachineEvent {
		out := []*corev1.MachineEvent{}
		for _, v := range versions {
			out = append(out, &corev1.MachineEvent{Version: v, Event: "state change"})
		}

		return out
	}
	numberedVersions := func(n int) []string {
		out := make([]string, 0, n)
		for i := range n {
			out = append(out, fmt.Sprintf("V%d", i))
		}

		return out
	}

	// Each case asserts a different property of the pruned Machine, so the check travels with the
	// input rather than a shared assertion block trying to cover all of them.
	tests := []struct {
		name    string
		machine *corev1.Machine
		check   func(*testing.T, *corev1.Machine)
	}{
		{
			name: "clears the deprecated twins and keeps status and config",
			machine: &corev1.Machine{
				Id:             &corev1.MachineId{Id: "machine-1"},
				Health:         &corev1.HealthReport{Source: "deprecated"},
				Capabilities:   &corev1.MachineCapabilitiesSet{},
				UpdateComplete: true,
				HwSku:          proto.String("deprecated-sku"),
				Status:         &corev1.MachineStatus{Health: &corev1.HealthReport{Source: "status"}},
				Config:         &corev1.MachineConfig{},
			},
			check: func(t *testing.T, machine *corev1.Machine) {
				assert.Nil(t, machine.Health)
				assert.Nil(t, machine.Capabilities)
				assert.Nil(t, machine.HwSku)
				assert.False(t, machine.UpdateComplete)
				// The replacements the REST layer actually reads have to survive.
				assert.Equal(t, "status", machine.GetStatus().GetHealth().GetSource())
				assert.NotNil(t, machine.GetConfig())
				assert.Equal(t, "machine-1", machine.GetId().GetId())
			},
		},
		{
			name:    "keeps a short history whole",
			machine: &corev1.Machine{StateVersion: "V58", Events: events("V56", "V57", "V58")},
			check: func(t *testing.T, machine *corev1.Machine) {
				assert.Len(t, machine.Events, 3)
			},
		},
		{
			name:    "keeps the newest events when the history is longer than the bound",
			machine: &corev1.Machine{StateVersion: "V29", Events: events(numberedVersions(30)...)},
			check: func(t *testing.T, machine *corev1.Machine) {
				assert.Len(t, machine.Events, maxPublishedMachineEvents)
				// Core reports oldest first, so the tail has to be the newest entries.
				assert.Equal(t, "V10", machine.Events[0].GetVersion())
				assert.Equal(t, "V29", machine.Events[maxPublishedMachineEvents-1].GetVersion())
			},
		},
		{
			// The REST layer dates the current state from this event, so dropping it would
			// silently empty a response field. Nothing guarantees Core orders the matching event
			// last.
			name:    "carries the current state version when it falls outside the newest events",
			machine: &corev1.Machine{StateVersion: "V0", Events: events(numberedVersions(30)...)},
			check: func(t *testing.T, machine *corev1.Machine) {
				assert.Len(t, machine.Events, maxPublishedMachineEvents+1)
				assert.Equal(t, "V0", machine.Events[0].GetVersion())
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pruneMachineForPublish(tt.machine)
			tt.check(t, tt.machine)
		})
	}
}

func TestManageMachineInventory_CollectAndPublishMachineInventory(t *testing.T) {
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
			name: "test collecting and publishing machine inventory, empty inventory",
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
			name: "test collecting and publishing machine inventory, normal inventory",
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

			mmi := &ManageMachineInventory{
				config: ManageInventoryConfig{
					SiteID:                tt.fields.siteID,
					CoreGrpcAtomicClient:  tt.fields.coreGrpcAtomicClient,
					TemporalPublishClient: tc,
					TemporalPublishQueue:  tt.fields.temporalPublishQueue,
					SitePageSize:          tt.fields.sitePageSize,
					CloudPageSize:         tt.fields.cloudPageSize,
				},
			}

			ctx := context.Background()
			ctx = context.WithValue(ctx, "wantCount", tt.args.wantTotalItems)

			totalPages := tt.args.wantTotalItems / tt.fields.cloudPageSize
			if tt.args.wantTotalItems%tt.fields.cloudPageSize > 0 {
				totalPages++
			}

			err := mmi.CollectAndPublishMachineInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.MachineInventory)
			assert.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.Machines))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.Machines))
			}

			assert.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, inventory.InventoryStatus)
			assert.Equal(t, totalPages, int(inventory.InventoryPage.TotalPages))
			assert.Equal(t, 1, int(inventory.InventoryPage.CurrentPage))
			assert.Equal(t, tt.fields.cloudPageSize, int(inventory.InventoryPage.PageSize))
			assert.Equal(t, tt.args.wantTotalItems, int(inventory.InventoryPage.TotalItems))
			assert.Equal(t, tt.args.wantTotalItems, len(inventory.InventoryPage.ItemIds))
		})
	}
}

func TestManageMachine_GetDpuMachinesByIDs(t *testing.T) {
	// Custom mock implementation that returns DPU machines
	type mockDpuCoreGrpcClient struct {
		cClient.MockCoreGrpcServiceClient
	}

	mockFindMachinesByIds := func(ctx context.Context, in *corev1.MachinesByIdsRequest, opts ...grpc.CallOption) (*corev1.MachineList, error) {
		out := &corev1.MachineList{}
		if in != nil {
			for _, id := range in.MachineIds {
				out.Machines = append(out.Machines, &corev1.Machine{
					Id:          id,
					State:       "Ready",
					MachineType: corev1.MachineType_DPU,
				})
			}
		}
		return out, nil
	}

	mockGetNetworkConfig := func(ctx context.Context, in *corev1.ManagedHostNetworkConfigRequest, opts ...grpc.CallOption) (*corev1.ManagedHostNetworkConfigResponse, error) {
		return &corev1.ManagedHostNetworkConfigResponse{}, nil
	}

	type args struct {
		ctx           context.Context
		dpuMachineIDs []string
	}
	tests := []struct {
		name             string
		args             args
		wantDpuCount     int
		wantErr          bool
		wantNonRetryable bool
	}{
		{
			name: "test GetDpuMachinesByIDs returns correct DPU machines with matching IDs",
			args: args{
				ctx:           context.Background(),
				dpuMachineIDs: []string{"dpu-machine-1", "dpu-machine-2", "dpu-machine-3"},
			},
			wantDpuCount:     3,
			wantErr:          false,
			wantNonRetryable: false,
		},
		{
			name: "test GetDpuMachinesByIDs handles single machine ID",
			args: args{
				ctx:           context.Background(),
				dpuMachineIDs: []string{"dpu-machine-single"},
			},
			wantDpuCount:     1,
			wantErr:          false,
			wantNonRetryable: false,
		},
		{
			name: "test GetDpuMachinesByIDs with multiple machines verifies all IDs",
			args: args{
				ctx:           context.Background(),
				dpuMachineIDs: []string{"dpu-a", "dpu-b", "dpu-c", "dpu-d", "dpu-e"},
			},
			wantDpuCount:     5,
			wantErr:          false,
			wantNonRetryable: false,
		},
		{
			name: "test GetDpuMachinesByIDs rejects empty machine IDs with non-retryable error",
			args: args{
				ctx:           context.Background(),
				dpuMachineIDs: []string{},
			},
			wantDpuCount:     0,
			wantErr:          true,
			wantNonRetryable: true,
		},
		{
			name: "test GetDpuMachinesByIDs rejects nil machine IDs with non-retryable error",
			args: args{
				ctx:           context.Background(),
				dpuMachineIDs: nil,
			},
			wantDpuCount:     0,
			wantErr:          true,
			wantNonRetryable: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create mock nico atomic client with our custom nico implementation
			baseAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
			baseClient := cClient.NewMockCoreGrpcClient()
			baseAtomicClient.SwapClient(baseClient)

			mm := &testManageMachineWithMock{
				ManageMachine: ManageMachine{
					coreGrpcAtomicClient: baseAtomicClient,
				},
				mockFindMachines: mockFindMachinesByIds,
				mockGetNetwork:   mockGetNetworkConfig,
			}

			got, err := mm.GetDpuMachinesByIDsWithMock(tt.args.ctx, tt.args.dpuMachineIDs)

			if tt.wantErr {
				assert.Error(t, err)
				if tt.wantNonRetryable {
					var appErr *temporal.ApplicationError
					if errors.As(err, &appErr) {
						assert.True(t, appErr.NonRetryable(), "Expected error to be non-retryable")
					}
				}
			} else {
				assert.NoError(t, err)
				assert.NotNil(t, got)
				assert.Equal(t, tt.wantDpuCount, len(got), "Expected %d DPU machines but got %d", tt.wantDpuCount, len(got))

				// Verify all returned DPU machines have correct structure and IDs
				returnedIDs := make(map[string]bool)
				for i, dpuMachine := range got {
					assert.NotNil(t, dpuMachine, "DPU machine at index %d should not be nil", i)
					assert.NotNil(t, dpuMachine.Machine, "DPU machine.Machine at index %d should not be nil", i)
					assert.Equal(t, corev1.MachineType_DPU, dpuMachine.Machine.MachineType,
						"DPU machine at index %d should have type DPU", i)
					assert.NotNil(t, dpuMachine.Machine.Id, "DPU machine ID at index %d should not be nil", i)
					assert.NotEmpty(t, dpuMachine.Machine.Id.Id, "DPU machine ID string at index %d should not be empty", i)
					assert.Equal(t, "Ready", dpuMachine.Machine.State, "DPU machine at index %d should be in Ready state", i)

					// Verify the machine ID matches one of the requested IDs
					machineID := dpuMachine.Machine.Id.Id
					found := false
					for _, requestedID := range tt.args.dpuMachineIDs {
						if machineID == requestedID {
							found = true
							break
						}
					}
					assert.True(t, found, "DPU machine ID '%s' at index %d should be in requested list %v",
						machineID, i, tt.args.dpuMachineIDs)

					// Track returned IDs to ensure no duplicates
					assert.False(t, returnedIDs[machineID], "DPU machine ID '%s' should not be returned twice", machineID)
					returnedIDs[machineID] = true

					// Network config should be present
					assert.NotNil(t, dpuMachine.DpuNetworkConfig,
						"DPU network config at index %d should not be nil", i)
				}

				// Verify all requested IDs were returned
				for _, requestedID := range tt.args.dpuMachineIDs {
					assert.True(t, returnedIDs[requestedID],
						"Requested DPU machine ID '%s' should be in the returned results", requestedID)
				}
			}
		})
	}
}

// testManageMachineWithMock wraps ManageMachine and overrides the gRPC calls for testing
type testManageMachineWithMock struct {
	ManageMachine
	mockFindMachines func(context.Context, *corev1.MachinesByIdsRequest, ...grpc.CallOption) (*corev1.MachineList, error)
	mockGetNetwork   func(context.Context, *corev1.ManagedHostNetworkConfigRequest, ...grpc.CallOption) (*corev1.ManagedHostNetworkConfigResponse, error)
}

// GetDpuMachinesByIDsWithMock is a test version that uses our mocked responses
func (mm *testManageMachineWithMock) GetDpuMachinesByIDsWithMock(ctx context.Context, dpuMachineIDs []string) ([]*corev1.DpuMachine, error) {
	logger := log.With().Str("Activity", "GetDpuMachinesByIDs").Logger()
	logger.Info().Msg("Starting activity")

	var err error

	// Validate request
	if len(dpuMachineIDs) == 0 {
		err = errors.New("received GetDpuMachinesByIDs request without DPU Machine IDs")
		return nil, temporal.NewNonRetryableApplicationError(err.Error(), "INVALID_REQUEST", err)
	}

	// Convert string IDs to MachineId objects
	machineIDs := make([]*corev1.MachineId, 0, len(dpuMachineIDs))
	for _, id := range dpuMachineIDs {
		machineIDs = append(machineIDs, &corev1.MachineId{Id: id})
	}

	request := &corev1.MachinesByIdsRequest{
		MachineIds: machineIDs,
	}

	// Use mock instead of real client
	machineList, err := mm.mockFindMachines(ctx, request)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve DPU Machines by IDs")
		return nil, err
	}

	// For each DPU machine, fetch the network configuration
	dpuMachines := make([]*corev1.DpuMachine, 0, len(machineList.Machines))
	for _, machine := range machineList.Machines {
		if machine.MachineType == corev1.MachineType_DPU {
			networkConfigReq := &corev1.ManagedHostNetworkConfigRequest{
				DpuMachineId: machine.Id,
			}
			networkConfig, nerr := mm.mockGetNetwork(ctx, networkConfigReq)
			if nerr != nil {
				logger.Warn().Err(nerr).Str("DPU Machine ID", machine.Id.Id).Msg("Failed to retrieve network config for DPU machine, continuing without it")
			} else {
				logger.Debug().Str("DPU Machine ID", machine.Id.Id).Msg("Retrieved network config for DPU machine")
			}
			dpuMachines = append(dpuMachines, &corev1.DpuMachine{
				Machine:          machine,
				DpuNetworkConfig: networkConfig,
			})
		}
	}

	logger.Info().Int("dpu_machine_count", len(dpuMachines)).Msg("Completed activity")
	return dpuMachines, nil
}
