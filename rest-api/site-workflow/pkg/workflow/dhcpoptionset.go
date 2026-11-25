// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

// DiscoverDhcpOptionSetInventory collects DhcpOptionSet inventory from Core and publishes it to Cloud.
func DiscoverDhcpOptionSetInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverDhcpOptionSetInventory").Logger()

	logger.Info().Msg("Starting workflow")

	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		// The inventory cron runs again shortly, so a few attempts are enough.
		MaximumAttempts: 2,
	}
	options := workflow.ActivityOptions{
		StartToCloseTimeout: 2 * time.Minute,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var dhcpOptionSetInventoryManager activity.ManageDhcpOptionSetInventory

	err := workflow.ExecuteActivity(ctx, dhcpOptionSetInventoryManager.DiscoverDhcpOptionSetInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverDhcpOptionSetInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
