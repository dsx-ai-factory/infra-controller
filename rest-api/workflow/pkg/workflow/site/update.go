// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"errors"
	"fmt"

	"github.com/rs/zerolog/log"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"go.temporal.io/sdk/workflow"

	siteActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/site"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
)

// UpdateSiteConfigInventory applies Site metadata and runtime configuration
// reported by the Site Agent. It stores the Core build version and advertised
// VPC SLAAC capability, then creates IP Blocks for Site fabric prefixes.
//
// Site Agents now publish UpdateSiteConfigInventoryV2. This stays registered for the rollout
// window, where Cloud upgrades ahead of the Site Agents still publishing V1.
func UpdateSiteConfigInventory(ctx workflow.Context, siteIDStr string, coreBuildInfo *corev1.BuildInfo) error {
	logger := log.With().Str("Workflow", "UpdateSiteConfigInventory").Str("SiteID", siteIDStr).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	siteID, err := uuid.Parse(siteIDStr)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteIDStr))
		return err
	}

	options := cwi.ActivityOptions()
	ctx = workflow.WithActivityOptions(ctx, options)

	var manageSite siteActivity.ManageSite

	// An older Site Agent reports no build info of its own, so the stored values are left alone.
	siteUpdateErr := workflow.ExecuteActivity(ctx, manageSite.UpdateSiteInDB, siteID, coreBuildInfo, nil).Get(ctx, nil)
	if siteUpdateErr != nil {
		logger.Warn().Err(siteUpdateErr).Msg("failed to execute UpdateSiteInDB activity")
	}

	siteFabricPrefixes := coreBuildInfo.GetRuntimeConfig().GetSiteFabricPrefixes()
	ipBlockUpdateErr := workflow.ExecuteActivity(ctx, manageSite.UpdateIPBlocksInDBFromFabricPrefixes, siteID, siteFabricPrefixes).Get(ctx, nil)
	if ipBlockUpdateErr != nil {
		logger.Warn().Err(ipBlockUpdateErr).Msg("failed to execute UpdateIPBlocksInDBFromFabricPrefixes activity")
	}

	inventoryErr := siteUpdateErr
	if inventoryErr == nil {
		inventoryErr = ipBlockUpdateErr
	} else if ipBlockUpdateErr != nil {
		inventoryErr = errors.Join(siteUpdateErr, ipBlockUpdateErr)
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, siteID, "UpdateSiteConfigInventory", inventoryErr != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return every inventory update error after both updates have been attempted.
	return inventoryErr
}

// UpdateSiteConfigInventoryV2 applies the Site configuration snapshot reported by the Site
// Agent. It carries everything UpdateSiteConfigInventory did, plus the build info the Site
// Agent reports about itself, so further Site-level reporting extends SiteConfigInventory
// rather than needing another workflow.
func UpdateSiteConfigInventoryV2(ctx workflow.Context, siteIDStr string, inventory *corev1.SiteConfigInventory) error {
	logger := log.With().Str("Workflow", "UpdateSiteConfigInventoryV2").Str("SiteID", siteIDStr).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	siteID, err := uuid.Parse(siteIDStr)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteIDStr))
		return err
	}

	options := cwi.ActivityOptions()
	ctx = workflow.WithActivityOptions(ctx, options)

	coreBuildInfo := inventory.GetCoreBuildInfo()

	var manageSite siteActivity.ManageSite

	siteUpdateErr := workflow.ExecuteActivity(ctx, manageSite.UpdateSiteInDB, siteID, coreBuildInfo,
		inventory.GetSiteAgentBuildInfo()).Get(ctx, nil)
	if siteUpdateErr != nil {
		logger.Warn().Err(siteUpdateErr).Msg("failed to execute UpdateSiteInDB activity")
	}

	siteFabricPrefixes := coreBuildInfo.GetRuntimeConfig().GetSiteFabricPrefixes()
	ipBlockUpdateErr := workflow.ExecuteActivity(ctx, manageSite.UpdateIPBlocksInDBFromFabricPrefixes, siteID, siteFabricPrefixes).Get(ctx, nil)
	if ipBlockUpdateErr != nil {
		logger.Warn().Err(ipBlockUpdateErr).Msg("failed to execute UpdateIPBlocksInDBFromFabricPrefixes activity")
	}

	inventoryErr := siteUpdateErr
	if inventoryErr == nil {
		inventoryErr = ipBlockUpdateErr
	} else if ipBlockUpdateErr != nil {
		inventoryErr = errors.Join(siteUpdateErr, ipBlockUpdateErr)
	}

	// Record latency under the V1 name so the series stays continuous as Sites move over.
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, siteID, "UpdateSiteConfigInventory", inventoryErr != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return every inventory update error after both updates have been attempted.
	return inventoryErr
}
