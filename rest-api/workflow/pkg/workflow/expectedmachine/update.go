// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"fmt"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	expectedMachineActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedmachine"
)

// UpdateExpectedMachineInventory is a workflow called by Site Agent to update ExpectedMachine inventory for a Site
func UpdateExpectedMachineInventory(ctx workflow.Context, siteID string, expectedMachineInventory *corev1.ExpectedMachineInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateExpectedMachineInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var expectedMachineManager expectedMachineActivity.ManageExpectedMachine

	err = workflow.ExecuteActivity(ctx, expectedMachineManager.UpdateExpectedMachinesInDB, parsedSiteID, expectedMachineInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateExpectedMachinesInDB")
		return err
	}

	logger.Info().Msg("completing workflow")

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	err = workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateExpectedMachineInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: RecordLatency")
	}

	return nil
}
