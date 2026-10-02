// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sitePrefixActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/siteprefix"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
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
	ctx = workflow.WithActivityOptions(ctx, options)
	var manager sitePrefixActivity.ManageSitePrefix
	err = workflow.ExecuteActivity(ctx, manager.UpdateSitePrefixesInDB, id, inventory).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateSitePrefixesInDB")
	}

	var metrics cwm.ManageInventoryMetrics
	metricsErr := workflow.ExecuteActivity(ctx, metrics.RecordLatency, id,
		"UpdateSitePrefixInventory", err != nil, workflow.Now(ctx).Sub(start)).Get(ctx, nil)
	if metricsErr != nil {
		logger.Warn().Err(metricsErr).Msg("failed to execute activity: RecordLatency")
	}
	logger.Info().Msg("completing workflow")
	return err
}
