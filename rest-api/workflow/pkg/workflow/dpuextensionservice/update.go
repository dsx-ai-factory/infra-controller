// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/dpuextensionservice"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateDpuExtensionServiceInventory is a workflow called by Site Agent to update Dpu Extension Service inventory for a Site
func UpdateDpuExtensionServiceInventory(ctx workflow.Context, siteID string, dpuExtensionServiceInventory *corev1.DpuExtensionServiceInventory) error {
	logger := log.With().Str("Workflow", "UpdateDpuExtensionServiceInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var DpuExtensionServiceManager dpuextensionservice.ManageDpuExtensionService

	err = workflow.ExecuteActivity(ctx, DpuExtensionServiceManager.UpdateDpuExtensionServicesInDB, parsedSiteID, dpuExtensionServiceInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateDpuExtensionServicesInDB")
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateDpuExtensionServiceInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return original error from inventory activity, if any
	return err
}
