// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dhcpoptionset

import (
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/workflow"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	dhcpOptionSetActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/dhcpoptionset"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateDhcpOptionSetInventory is a workflow called by Site Agent to update DhcpOptionSet inventory for a Site
func UpdateDhcpOptionSetInventory(ctx workflow.Context, siteID string, dhcpOptionSetInventory *corev1.DhcpOptionSetInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateDhcpOptionSetInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	ctx = workflow.WithActivityOptions(ctx, cwi.ActivityOptions())

	var dhcpOptionSetManager dhcpOptionSetActivity.ManageDhcpOptionSet

	err = workflow.ExecuteActivity(ctx, dhcpOptionSetManager.UpdateDhcpOptionSetsInDB, parsedSiteID, dhcpOptionSetInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateDhcpOptionSetsInDB")
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateDhcpOptionSetInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return original error from inventory activity, if any
	return err
}
