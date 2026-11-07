// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	expectedRackGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedrackgroup"
)

// UpdateExpectedRackGroupInventory is a workflow called by Site Agent to update ExpectedRackGroup inventory for a Site
func UpdateExpectedRackGroupInventory(ctx workflow.Context, siteID string, expectedRackGroupInventory *corev1.ExpectedRackGroupInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateExpectedRackGroupInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var expectedRackGroupManager expectedRackGroupActivity.ManageExpectedRackGroup

	err = workflow.ExecuteActivity(ctx, expectedRackGroupManager.UpdateExpectedRackGroupsInDB, parsedSiteID, expectedRackGroupInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateExpectedRackGroupsInDB")
		return err
	}

	logger.Info().Msg("completing workflow")

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	err = workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateExpectedRackGroupInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: RecordLatency")
	}

	return nil
}
