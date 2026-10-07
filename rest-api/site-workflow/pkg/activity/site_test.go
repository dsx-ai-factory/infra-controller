// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/durationpb"
)

func TestManageSiteConfigInventory_DiscoverSiteConfigInventory(t *testing.T) {
	siteFabricPrefixes := []string{"10.0.0.0/16", "2001:db8::/64"}
	buildCapabilities := []corev1.BuildCapability{
		corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC,
	}

	tests := []struct {
		name               string
		coreClientMissing  bool
		siteAgentBuildInfo *corev1.SiteAgentBuildInfo
		wantErr            error
	}{
		{
			name: "publishes Site Agent build info",
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{
				Version:           "2.0.0",
				InventoryInterval: durationpb.New(3 * time.Minute),
				FlowEnabled:       proto.Bool(true),
			},
		},
		{
			// The Site Agent leaves the interval unset when it cannot derive one, so Cloud can
			// tell that apart from a real value and stay on its own default.
			name:               "publishes an absent inventory interval",
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: "2.0.0"},
		},
		{
			name:               "fails when the Core gRPC client is not connected",
			coreClientMissing:  true,
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: "2.0.0"},
			wantErr:            cClient.ErrCoreGrpcClientNotConnected,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
			if !tt.coreClientMissing {
				mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()
				coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

				mockCoreService, ok := mockCoreGrpcClient.GrpcServiceClient().(*cClient.MockCoreGrpcServiceClient)
				require.True(t, ok)
				mockCoreService.BuildCapabilities = buildCapabilities
			}

			siteID := uuid.New()
			wrun := &tmocks.WorkflowRun{}
			wrun.On("GetID").Return("test-workflow-id")

			tc := &tmocks.Client{}
			tc.Mock.On(
				"ExecuteWorkflow",
				mock.Anything,
				mock.AnythingOfType("internal.StartWorkflowOptions"),
				updateSiteConfigInventoryWorkflowName,
				siteID.String(),
				mock.Anything,
			).Return(wrun, nil)

			manageSiteConfigInventory := NewManageSiteConfigInventory(ManageInventoryConfig{
				SiteID:                siteID,
				CoreGrpcAtomicClient:  coreGrpcAtomicClient,
				TemporalPublishClient: tc,
				TemporalPublishQueue:  "test-queue",
			}, tt.siteAgentBuildInfo)

			ctx := context.WithValue(context.Background(), "siteFabricPrefixes", siteFabricPrefixes)
			err := manageSiteConfigInventory.DiscoverSiteConfigInventory(ctx)
			if tt.wantErr != nil {
				assert.ErrorIs(t, err, tt.wantErr)
				tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
				return
			}
			require.NoError(t, err)

			tc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			executeCtx, ok := tc.Calls[0].Arguments[0].(context.Context)
			require.True(t, ok)
			assert.Same(t, ctx, executeCtx)

			workflowOptions, ok := tc.Calls[0].Arguments[1].(tClient.StartWorkflowOptions)
			require.True(t, ok)
			assert.Equal(t, "update-site-config-inventory-"+siteID.String(), workflowOptions.ID)
			assert.Equal(t, "test-queue", workflowOptions.TaskQueue)

			inventory, ok := tc.Calls[0].Arguments[4].(*corev1.SiteConfigInventory)
			require.True(t, ok)

			buildInfo := inventory.GetCoreBuildInfo()
			require.NotNil(t, buildInfo)
			assert.Equal(t, buildCapabilities, buildInfo.GetCapabilities())
			require.NotNil(t, buildInfo.GetRuntimeConfig(),
				"Version request must set DisplayConfig, Core omits the runtime config without it")
			assert.Equal(t, siteFabricPrefixes, buildInfo.GetRuntimeConfig().GetSiteFabricPrefixes())

			assert.True(t, proto.Equal(tt.siteAgentBuildInfo, inventory.GetSiteAgentBuildInfo()))
		})
	}
}
