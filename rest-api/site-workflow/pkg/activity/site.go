// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"fmt"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/rs/zerolog/log"
	tClient "go.temporal.io/sdk/client"
)

const updateSiteConfigInventoryWorkflowName = "UpdateSiteConfigInventoryV2"

// ManageSiteConfigInventory is an activity wrapper for Site Config inventory collection and publishing.
type ManageSiteConfigInventory struct {
	inventoryConfig ManageInventoryConfig
	// siteAgentBuildInfo describes the Site Agent itself and is fixed for the life of the
	// process, so the caller builds it once rather than the activity rebuilding it per run.
	siteAgentBuildInfo *corev1.SiteAgentBuildInfo
}

// NewManageSiteConfigInventory returns a ManageSiteConfigInventory implementation.
func NewManageSiteConfigInventory(inventoryConfig ManageInventoryConfig, siteAgentBuildInfo *corev1.SiteAgentBuildInfo) ManageSiteConfigInventory {
	return ManageSiteConfigInventory{
		inventoryConfig:    inventoryConfig,
		siteAgentBuildInfo: siteAgentBuildInfo,
	}
}

// DiscoverSiteConfigInventory collects Core build metadata, advertised
// capabilities, and runtime configuration, alongside the Site Agent's own build info. It
// publishes that snapshot to the Cloud workflow, which stores the Core version and SLAAC
// capability, records the Site Agent build info, and creates IP Blocks for the Site fabric
// prefixes.
func (msi *ManageSiteConfigInventory) DiscoverSiteConfigInventory(ctx context.Context) error {
	logger := log.With().Str("Activity", "DiscoverSiteConfigInventory").Logger()
	logger.Info().Msg("Starting activity")

	grpcClient := msi.inventoryConfig.CoreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}

	coreBuildInfo, err := grpcClient.GrpcServiceClient().Version(ctx, &corev1.VersionRequest{
		DisplayConfig: true,
	})
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve Site runtime config using Core gRPC API")
		return err
	}

	inventory := &corev1.SiteConfigInventory{
		CoreBuildInfo:      coreBuildInfo,
		SiteAgentBuildInfo: msi.siteAgentBuildInfo,
	}

	workflowOptions := tClient.StartWorkflowOptions{
		ID:        fmt.Sprintf("update-site-config-inventory-%s", msi.inventoryConfig.SiteID.String()),
		TaskQueue: msi.inventoryConfig.TemporalPublishQueue,
	}

	if _, err = msi.inventoryConfig.TemporalPublishClient.ExecuteWorkflow(
		ctx,
		workflowOptions,
		updateSiteConfigInventoryWorkflowName,
		msi.inventoryConfig.SiteID.String(),
		inventory,
	); err != nil {
		logger.Error().Err(err).Msg("Failed to publish Site Config inventory to Cloud")
		return err
	}

	logger.Info().Msg("Completed activity")
	return nil
}
