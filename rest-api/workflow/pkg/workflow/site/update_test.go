// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/durationpb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	siteActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/site"
)

func TestUpdateSiteConfigInventory(t *testing.T) {
	type testCase struct {
		name                 string
		siteIDStr            string
		prefixes             []string
		buildVersion         string
		updateSiteInDBErr    error
		updateIPBlocksErr    error
		wantErr              bool
		wantErrContains      []string
		expectUpdateSiteInDB bool
		expectUpdateIPBlocks bool
		expectRecordLatency  bool
		recordLatencyFailed  bool
	}

	tests := []testCase{
		{
			name:                 "Success",
			prefixes:             []string{"10.0.0.0/16", "2001:db8::/64"},
			buildVersion:         "1.2.3",
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
		},
		{
			name:                 "UpdateIPBlocksInDBFails",
			prefixes:             []string{"10.0.0.0/16"},
			buildVersion:         "1.2.3",
			updateIPBlocksErr:    errors.New("failed to update Site IP Blocks"),
			wantErr:              true,
			wantErrContains:      []string{"failed to update Site IP Blocks"},
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
			recordLatencyFailed:  true,
		},
		{
			// UpdateSiteInDB failures do not stop the workflow from creating Site
			// fabric IP Blocks, but they still fail the inventory workflow.
			name:                 "UpdateSiteInDBFailsContinues",
			prefixes:             []string{"10.0.0.0/16"},
			buildVersion:         "1.2.3",
			updateSiteInDBErr:    errors.New("failed to update Site metadata"),
			wantErr:              true,
			wantErrContains:      []string{"failed to update Site metadata"},
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
			recordLatencyFailed:  true,
		},
		{
			name:              "BothInventoryUpdatesFail",
			prefixes:          []string{"10.0.0.0/16"},
			buildVersion:      "1.2.3",
			updateSiteInDBErr: errors.New("failed to update Site metadata"),
			updateIPBlocksErr: errors.New("failed to update Site IP Blocks"),
			wantErr:           true,
			wantErrContains: []string{
				"failed to update Site metadata",
				"failed to update Site IP Blocks",
			},
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
			recordLatencyFailed:  true,
		},
		{
			name:      "InvalidSiteID",
			siteIDStr: "not-a-site-id",
			wantErr:   true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var ts testsuite.WorkflowTestSuite
			env := ts.NewTestWorkflowEnvironment()
			t.Cleanup(func() {
				env.AssertExpectations(t)
			})

			siteIDStr := tt.siteIDStr
			var siteID uuid.UUID
			if siteIDStr == "" {
				siteID = uuid.New()
				siteIDStr = siteID.String()
			}

			buildInfo := &corev1.BuildInfo{
				BuildVersion: tt.buildVersion,
			}
			if tt.prefixes != nil {
				buildInfo.RuntimeConfig = &corev1.RuntimeConfig{
					SiteFabricPrefixes: tt.prefixes,
				}
			}

			var siteManager siteActivity.ManageSite
			var metricsManager cwm.ManageInventoryMetrics

			if tt.expectUpdateSiteInDB {
				env.RegisterActivity(siteManager.UpdateSiteInDB)
				// V1 carries no Site Agent build info, so the activity receives a typed nil.
				env.OnActivity(siteManager.UpdateSiteInDB, mock.Anything, siteID, buildInfo,
					(*corev1.SiteAgentBuildInfo)(nil)).Return(tt.updateSiteInDBErr)
			}
			if tt.expectUpdateIPBlocks {
				env.RegisterActivity(siteManager.UpdateIPBlocksInDBFromFabricPrefixes)
				env.OnActivity(siteManager.UpdateIPBlocksInDBFromFabricPrefixes, mock.Anything, siteID, tt.prefixes).Return(tt.updateIPBlocksErr)
			}
			if tt.expectRecordLatency {
				env.RegisterActivity(metricsManager.RecordLatency)
				env.OnActivity(
					metricsManager.RecordLatency,
					mock.Anything,
					siteID,
					"UpdateSiteConfigInventory",
					tt.recordLatencyFailed,
					mock.Anything,
				).Return(nil)
			}

			env.ExecuteWorkflow(UpdateSiteConfigInventory, siteIDStr, buildInfo)
			require.True(t, env.IsWorkflowCompleted())

			err := env.GetWorkflowError()
			if !tt.wantErr {
				require.NoError(t, err)
				return
			}

			require.Error(t, err)
			for _, wantErr := range tt.wantErrContains {
				assert.ErrorContains(t, err, wantErr)
			}
		})
	}
}

func TestUpdateSiteConfigInventoryV2(t *testing.T) {
	type testCase struct {
		name                 string
		siteIDStr            string
		prefixes             []string
		buildVersion         string
		siteAgentBuildInfo   *corev1.SiteAgentBuildInfo
		updateSiteInDBErr    error
		updateIPBlocksErr    error
		wantErr              bool
		wantErrContains      []string
		expectUpdateSiteInDB bool
		expectUpdateIPBlocks bool
		expectRecordLatency  bool
		recordLatencyFailed  bool
	}

	tests := []testCase{
		{
			name:         "Success",
			prefixes:     []string{"10.0.0.0/16", "2001:db8::/64"},
			buildVersion: "1.2.3",
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{
				Version:           "2.0.0",
				InventoryInterval: durationpb.New(time.Minute),
				FlowEnabled:       proto.Bool(true),
			},
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
		},
		{
			// A Site Agent that reports nothing about itself still updates the Core-reported
			// fields, and the activity decides to leave the Site Agent values alone.
			name:                 "NoSiteAgentBuildInfo",
			prefixes:             []string{"10.0.0.0/16"},
			buildVersion:         "1.2.3",
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
		},
		{
			// UpdateSiteInDB failures do not stop the workflow from creating Site fabric IP
			// Blocks, but they still fail the inventory workflow.
			name:                 "UpdateSiteInDBFailsContinues",
			prefixes:             []string{"10.0.0.0/16"},
			buildVersion:         "1.2.3",
			siteAgentBuildInfo:   &corev1.SiteAgentBuildInfo{Version: "2.0.0"},
			updateSiteInDBErr:    errors.New("failed to update Site metadata"),
			wantErr:              true,
			wantErrContains:      []string{"failed to update Site metadata"},
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
			recordLatencyFailed:  true,
		},
		{
			name:               "BothInventoryUpdatesFail",
			prefixes:           []string{"10.0.0.0/16"},
			buildVersion:       "1.2.3",
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: "2.0.0"},
			updateSiteInDBErr:  errors.New("failed to update Site metadata"),
			updateIPBlocksErr:  errors.New("failed to update Site IP Blocks"),
			wantErr:            true,
			wantErrContains: []string{
				"failed to update Site metadata",
				"failed to update Site IP Blocks",
			},
			expectUpdateSiteInDB: true,
			expectUpdateIPBlocks: true,
			expectRecordLatency:  true,
			recordLatencyFailed:  true,
		},
		{
			name:      "InvalidSiteID",
			siteIDStr: "not-a-site-id",
			wantErr:   true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var ts testsuite.WorkflowTestSuite
			env := ts.NewTestWorkflowEnvironment()
			t.Cleanup(func() {
				env.AssertExpectations(t)
			})

			siteIDStr := tt.siteIDStr
			var siteID uuid.UUID
			if siteIDStr == "" {
				siteID = uuid.New()
				siteIDStr = siteID.String()
			}

			buildInfo := &corev1.BuildInfo{
				BuildVersion: tt.buildVersion,
			}
			if tt.prefixes != nil {
				buildInfo.RuntimeConfig = &corev1.RuntimeConfig{
					SiteFabricPrefixes: tt.prefixes,
				}
			}
			inventory := &corev1.SiteConfigInventory{
				CoreBuildInfo:      buildInfo,
				SiteAgentBuildInfo: tt.siteAgentBuildInfo,
			}

			var siteManager siteActivity.ManageSite
			var metricsManager cwm.ManageInventoryMetrics

			if tt.expectUpdateSiteInDB {
				env.RegisterActivity(siteManager.UpdateSiteInDB)
				// A message without Site Agent build info reaches the activity as a typed nil,
				// which is what the pointer carries here when the case leaves it unset.
				env.OnActivity(siteManager.UpdateSiteInDB, mock.Anything, siteID, buildInfo,
					tt.siteAgentBuildInfo).Return(tt.updateSiteInDBErr)
			}
			if tt.expectUpdateIPBlocks {
				env.RegisterActivity(siteManager.UpdateIPBlocksInDBFromFabricPrefixes)
				env.OnActivity(siteManager.UpdateIPBlocksInDBFromFabricPrefixes, mock.Anything, siteID, tt.prefixes).Return(tt.updateIPBlocksErr)
			}
			if tt.expectRecordLatency {
				env.RegisterActivity(metricsManager.RecordLatency)
				env.OnActivity(
					metricsManager.RecordLatency,
					mock.Anything,
					siteID,
					// V2 reports under the V1 name so the latency series stays continuous.
					"UpdateSiteConfigInventory",
					tt.recordLatencyFailed,
					mock.Anything,
				).Return(nil)
			}

			env.ExecuteWorkflow(UpdateSiteConfigInventoryV2, siteIDStr, inventory)
			require.True(t, env.IsWorkflowCompleted())

			err := env.GetWorkflowError()
			if !tt.wantErr {
				require.NoError(t, err)
				return
			}

			require.Error(t, err)
			for _, wantErr := range tt.wantErrContains {
				assert.ErrorContains(t, err, wantErr)
			}
		})
	}
}
