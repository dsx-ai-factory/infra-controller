// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operatingsystem

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	osActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/operatingsystem"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateOsImageInventory is a workflow called by Site Agent to update image based Operating System for a Site
func UpdateOsImageInventory(ctx workflow.Context, siteID string, osImageInventory *corev1.OsImageInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateOsImageInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var osManager osActivity.ManageOsImage

	var osIDs []uuid.UUID

	err = workflow.ExecuteActivity(ctx, osManager.UpdateOsImagesInDB, parsedSiteID, osImageInventory).Get(ctx, &osIDs)
	if err != nil {
		logger.Warn().Err(err).Msg("failed execute activity: UpdateOsImagesInDB")
	} else {
		// Update the status of the corresponding Operating Systems
		for _, osID := range osIDs {
			serr := workflow.ExecuteActivity(ctx, osManager.UpdateOperatingSystemStatusInDB, osID).Get(ctx, nil)
			if serr != nil {
				// Log error but continue as we don't want to interrupt inventory processing
				logger.Warn().Err(serr).Msg("failed to execute activity: UpdateOperatingSystemStatusInDB")
			}
		}
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateOsImageInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	return err
}

// UpdateOperatingSystemInventory is a workflow called by the Site Agent to reconcile Operating Systems
// synced from nico-core into the operating_system table.
func UpdateOperatingSystemInventory(ctx workflow.Context, siteID string, inventory *corev1.OperatingSystemInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateOperatingSystemInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("Starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msgf("workflow triggered with invalid site ID: %s", siteID)
		return err
	}

	options := cwi.ActivityOptions()
	ctx = workflow.WithActivityOptions(ctx, options)

	var osManager osActivity.ManageOsImage

	// UpdateOperatingSystemsInDB reconciles the inventory and returns only an
	// error (no IDs), so decode into a nil result target.
	err = workflow.ExecuteActivity(ctx, osManager.UpdateOperatingSystemsInDB, parsedSiteID, inventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to execute activity: UpdateOperatingSystemsInDB")
	}

	logger.Info().Msg("Completing workflow")

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateOperatingSystemInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("Failed to execute activity: RecordLatency")
	}

	return err
}
