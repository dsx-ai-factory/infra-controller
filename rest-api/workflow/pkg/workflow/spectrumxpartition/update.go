// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package spectrumxpartition

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	sxpActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/spectrumxpartition"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateSpectrumXPartitionInventory is a workflow called by Site Agent to update
// SpectrumXPartition inventory for a Site
func UpdateSpectrumXPartitionInventory(ctx workflow.Context, siteID string, sxpInventory *corev1.SpectrumXPartitionInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateSpectrumXPartitionInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var sxpManager sxpActivity.ManageSpectrumXPartition

	err = workflow.ExecuteActivity(ctx, sxpManager.UpdateSpectrumXPartitionsInDB, parsedSiteID, sxpInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateSpectrumXPartitionsInDB")
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateSpectrumXPartitionInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return original error from inventory activity, if any
	return err
}
