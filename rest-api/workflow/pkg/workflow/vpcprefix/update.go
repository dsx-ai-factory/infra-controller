// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	vpcPrefixActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpcprefix"
)

// UpdateVpcPrefixInventory is a workflow called by Site Agent to update vpc prefixes for a Site
func UpdateVpcPrefixInventory(ctx workflow.Context, siteID string, vpcPrefixInventory *corev1.VpcPrefixInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateVpcPrefixInventory").Str("Site ID", siteID).Logger()

	logger.Info().Msg("starting workflow")

	startTime := workflow.Now(ctx)

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var vpcPrefixManager vpcPrefixActivity.ManageVpcPrefix

	err = workflow.ExecuteActivity(ctx, vpcPrefixManager.UpdateVpcPrefixesInDB, parsedSiteID, vpcPrefixInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed execute activity: UpdateVpcPrefixesInDB")
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateVpcPrefixInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return original error from inventory activity, if any
	return err
}
