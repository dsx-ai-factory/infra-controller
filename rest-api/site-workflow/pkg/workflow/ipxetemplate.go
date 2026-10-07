// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

// DiscoverIpxeTemplateInventory is a workflow that triggers iPXE template inventory
// collection from the Site Controller and publishes it to the cloud.
func DiscoverIpxeTemplateInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverIpxeTemplateInventory").Logger()
	logger.Info().Msg("Starting workflow")

	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		// Executed every 3 minutes, so we don't want too many retry attempts
		MaximumAttempts: 2,
	}
	options := workflow.ActivityOptions{
		StartToCloseTimeout: 2 * time.Minute,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var inventoryManager activity.ManageIpxeTemplateInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverIpxeTemplateInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverIpxeTemplateInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")
	return nil
}
