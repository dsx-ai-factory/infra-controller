// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sitePrefixActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/siteprefix"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

// UpdateSitePrefixInventory receives one Core inventory page without inferring
// absence or enabling Site Agent publication.
func UpdateSitePrefixInventory(ctx workflow.Context, siteID string, inventory *corev1.SitePrefixInventory) error {
	logger := log.With().Str("Workflow", "UpdateSitePrefixInventory").Str("Site ID", siteID).Logger()
	logger.Info().Msg("starting workflow")

	start := workflow.Now(ctx)
	id, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg("workflow triggered with invalid site ID")
		return err
	}
	options := cwi.ActivityOptions()
	// Schedule-to-close includes queue time and both attempts (125s worst-case
	// execution). Leave room for metrics and workflow bookkeeping within 3m.
	options.ScheduleToCloseTimeout = 140 * time.Second
	if inventory.GetInventoryStatus() == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		// Diagnostic sends have a separate 30s execution budget.
		options.StartToCloseTimeout = 15 * time.Second
		options.ScheduleToCloseTimeout = 15 * time.Second
		options.RetryPolicy = &temporal.RetryPolicy{MaximumAttempts: 1}
	}
	activityCtx := workflow.WithActivityOptions(ctx, options)
	var manager sitePrefixActivity.ManageSitePrefix
	err = workflow.ExecuteActivity(activityCtx, manager.UpdateSitePrefixesInDB, id, inventory).Get(activityCtx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateSitePrefixesInDB")
	}

	metricsCtx := workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 5 * time.Second, ScheduleToCloseTimeout: 5 * time.Second,
		RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1},
	})
	var metrics cwm.ManageInventoryMetrics
	metricsErr := workflow.ExecuteActivity(metricsCtx, metrics.RecordLatency, id,
		"UpdateSitePrefixInventory", err != nil, workflow.Now(ctx).Sub(start)).Get(metricsCtx, nil)
	if metricsErr != nil {
		logger.Warn().Err(metricsErr).Msg("failed to execute activity: RecordLatency")
	}
	logger.Info().Msg("completing workflow")
	return err
}
