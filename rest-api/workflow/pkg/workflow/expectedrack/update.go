// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrack

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	expectedRackActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedrack"
)

// UpdateExpectedRackInventory is a workflow called by Site Agent to update ExpectedRack inventory for a Site
func UpdateExpectedRackInventory(ctx workflow.Context, siteID string, expectedRackInventory *corev1.ExpectedRackInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateExpectedRackInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var expectedRackManager expectedRackActivity.ManageExpectedRack

	err = workflow.ExecuteActivity(ctx, expectedRackManager.UpdateExpectedRacksInDB, parsedSiteID, expectedRackInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateExpectedRacksInDB")
		return err
	}

	logger.Info().Msg("completing workflow")

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	err = workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateExpectedRackInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: RecordLatency")
	}

	return nil
}
