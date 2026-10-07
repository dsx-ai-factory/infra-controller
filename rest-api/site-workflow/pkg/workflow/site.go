// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	temporallog "go.temporal.io/sdk/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

// DiscoverSiteConfigInventory collects Core build metadata, advertised
// capabilities, and runtime configuration. It publishes that snapshot to the
// Cloud workflow, which stores the Core version and SLAAC capability and
// creates IP Blocks for the Site fabric prefixes.
func DiscoverSiteConfigInventory(ctx workflow.Context) error {
	logger := temporallog.With(workflow.GetLogger(ctx), "Workflow", "DiscoverSiteConfigInventory")

	logger.Info("Starting workflow")

	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		MaximumAttempts:    2,
	}
	options := workflow.ActivityOptions{
		StartToCloseTimeout: 2 * time.Minute,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var inventoryManager activity.ManageSiteConfigInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverSiteConfigInventory).Get(ctx, nil)
	if err != nil {
		logger.Error("Failed to execute activity from workflow", "Activity", "DiscoverSiteConfigInventory", "Error", err)
		return err
	}

	logger.Info("Completing workflow")

	return nil
}
