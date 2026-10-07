// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ipxetemplate

import (
	"fmt"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	ipxeTemplateActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/ipxetemplate"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"
)

// UpdateIpxeTemplateInventory is a workflow called by the Site Agent to update iPXE template
// inventory for a Site
func UpdateIpxeTemplateInventory(ctx workflow.Context, siteID string, inventory *corev1.IpxeTemplateInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateIpxeTemplateInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("Starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var templateManager ipxeTemplateActivity.ManageIpxeTemplate

	err = workflow.ExecuteActivity(ctx, templateManager.UpdateIpxeTemplatesInDB, parsedSiteID, inventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to execute activity: UpdateIpxeTemplatesInDB")
		return err
	}

	logger.Info().Msg("Completing workflow")

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	err = workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateIpxeTemplateInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to execute activity: RecordLatency")
	}

	return nil
}
