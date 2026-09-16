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
)

// TestManageSpectrumXPartitionInventory_DiscoverSpectrumXPartitionInventory proves the
// collector reaches Core through the SPX Find RPCs, pages the result, and names the Cloud
// workflow the reconciler is registered under. A wrong itemType would publish to a workflow
// nothing listens on, and the paging metadata is what Cloud keys its deletion sweep on.
func TestManageSpectrumXPartitionInventory_DiscoverSpectrumXPartitionInventory(t *testing.T) {
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
			name: "test collecting and publishing spectrumx partition inventory, empty inventory",
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
			name: "test collecting and publishing spectrumx partition inventory, normal inventory",
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

			manageInventory := NewManageSpectrumXPartitionInventory(ManageInventoryConfig{
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

			err := manageInventory.DiscoverSpectrumXPartitionInventory(ctx)
			assert.NoError(t, err)

			if tt.args.wantTotalItems == 0 {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			} else {
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)
			}

			// The published workflow name is derived from the itemType, so this is what
			// keeps the collector pointed at the Cloud reconciler.
			workflowName, ok := tc.Calls[0].Arguments[2].(string)
			require.True(t, ok)
			assert.Equal(t, "UpdateSpectrumXPartitionInventory", workflowName)

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.SpectrumXPartitionInventory)
			require.True(t, ok)

			if tt.args.wantTotalItems == 0 {
				assert.Equal(t, 0, len(inventory.SpxPartitions))
			} else {
				assert.Equal(t, tt.fields.cloudPageSize, len(inventory.SpxPartitions))
			}

			assert.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, inventory.InventoryStatus)
			assert.Equal(t, totalPages, int(inventory.InventoryPage.TotalPages))
			assert.Equal(t, 1, int(inventory.InventoryPage.CurrentPage))
			assert.Equal(t, tt.fields.cloudPageSize, int(inventory.InventoryPage.PageSize))
			assert.Equal(t, tt.args.wantTotalItems, int(inventory.InventoryPage.TotalItems))
			// Every page carries the full ID set, which is what Cloud compares against
			// its own rows to find Partitions that disappeared from the Site.
			assert.Equal(t, tt.args.wantTotalItems, len(inventory.InventoryPage.ItemIds))
		})
	}
}

// TestManageSpectrumXPartitionInventory_CollectionFailure proves a Core failure is reported
// onward as a failed inventory status rather than returning silently, so Cloud does not keep
// serving the previous data with no signal that the run failed.
func TestManageSpectrumXPartitionInventory_CollectionFailure(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return("test-workflow-id")

	tc := &tmocks.Client{}
	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("string"), mock.AnythingOfType("uuid.UUID"), mock.Anything).Return(wrun, nil)

	manageInventory := NewManageSpectrumXPartitionInventory(ManageInventoryConfig{
		SiteID:                uuid.New(),
		CoreGrpcAtomicClient:  coreGrpcAtomicClient,
		TemporalPublishClient: tc,
		TemporalPublishQueue:  "test-queue",
		SitePageSize:          100,
		CloudPageSize:         25,
	})

	ctx := context.WithValue(context.Background(), "wantError", assert.AnError)

	err := manageInventory.DiscoverSpectrumXPartitionInventory(ctx)
	assert.Error(t, err)

	// The failure is published exactly once so the second error cannot mask the first.
	tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)

	inventory, ok := tc.Calls[0].Arguments[4].(*corev1.SpectrumXPartitionInventory)
	require.True(t, ok)
	assert.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_FAILED, inventory.InventoryStatus)
	assert.NotEmpty(t, inventory.StatusMsg)
}
