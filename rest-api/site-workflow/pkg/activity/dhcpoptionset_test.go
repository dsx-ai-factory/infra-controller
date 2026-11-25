// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

// The collector must reach Core through the DhcpOptionSet find RPCs, page the result, and
// publish to the Cloud workflow named after its item type.
func TestManageDhcpOptionSetInventory_DiscoverDhcpOptionSetInventory(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()
	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return("test-workflow-id")

	const cloudPageSize = 25

	tests := []struct {
		name           string
		wantTotalItems int
	}{
		{name: "empty inventory", wantTotalItems: 0},
		{name: "inventory across several pages", wantTotalItems: 60},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tc := &tmocks.Client{}
			tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
				mock.AnythingOfType("string"), mock.AnythingOfType("uuid.UUID"), mock.Anything).Return(wrun, nil)

			manageInventory := NewManageDhcpOptionSetInventory(ManageInventoryConfig{
				SiteID:                uuid.New(),
				CoreGrpcAtomicClient:  coreGrpcAtomicClient,
				TemporalPublishClient: tc,
				TemporalPublishQueue:  "test-queue",
				SitePageSize:          100,
				CloudPageSize:         cloudPageSize,
			})

			ctx := context.WithValue(context.Background(), cClient.MockWantCount, tt.wantTotalItems)
			require.NoError(t, manageInventory.DiscoverDhcpOptionSetInventory(ctx))

			totalPages := (tt.wantTotalItems + cloudPageSize - 1) / cloudPageSize
			if totalPages == 0 {
				totalPages = 1
			}
			tc.AssertNumberOfCalls(t, "ExecuteWorkflow", totalPages)

			workflowName, ok := tc.Calls[0].Arguments[2].(string)
			require.True(t, ok)
			assert.Equal(t, "UpdateDhcpOptionSetInventory", workflowName)

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.DhcpOptionSetInventory)
			require.True(t, ok)
			assert.Equal(t, min(tt.wantTotalItems, cloudPageSize), len(inventory.DhcpOptionSets))
			assert.Equal(t, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, inventory.InventoryStatus)
			assert.Equal(t, tt.wantTotalItems, int(inventory.InventoryPage.TotalItems))
		})
	}
}
