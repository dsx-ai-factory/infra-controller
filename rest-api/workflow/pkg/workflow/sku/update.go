// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sku

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	skuActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sku"
)

// UpdateSkuInventory is a workflow called by Site Agent to update SKU inventory for a Site
func UpdateSkuInventory(ctx workflow.Context, siteID string, skuInventory *corev1.SkuInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateSkuInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var skuManager skuActivity.ManageSku

	err = workflow.ExecuteActivity(ctx, skuManager.UpdateSkusInDB, parsedSiteID, skuInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateSkusInDB")
		return err
	}

	logger.Info().Msg("completing workflow")

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	err = workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateSkuInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: RecordLatency")
	}

	return nil
}
