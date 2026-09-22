// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import (
	"fmt"

	"github.com/google/uuid"

	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	machineActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/machine"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
)

// UpdateMachineInventory is a workflow called by Site Agent to update machine inventory for a Site
func UpdateMachineInventory(ctx workflow.Context, siteID string, machineInventory *corev1.MachineInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateMachineInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	options := cwi.ActivityOptions()

	ctx = workflow.WithActivityOptions(ctx, options)

	var machineManager machineActivity.ManageMachine

	err = workflow.ExecuteActivity(ctx, machineManager.UpdateMachinesInDB, siteID, machineInventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateMachinesInDB")
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateMachineInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return original error from inventory activity, if any
	return err
}
