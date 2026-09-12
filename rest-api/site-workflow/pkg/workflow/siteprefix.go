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

// DiscoverSitePrefixInventory runs the activity that collects and publishes SitePrefix inventory.
func DiscoverSitePrefixInventory(ctx workflow.Context) error {
	logger := temporallog.With(workflow.GetLogger(ctx), "Workflow", "DiscoverSitePrefixInventory")
	logger.Info("Starting workflow")

	retryPolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		MaximumAttempts:    2,
	}
	ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 2 * time.Minute,
		RetryPolicy:         retryPolicy,
	})

	var inventoryManager activity.ManageSitePrefixInventory
	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverSitePrefixInventory).Get(ctx, nil)
	if err != nil {
		logger.Error("Failed to execute activity from workflow", "Activity", "DiscoverSitePrefixInventory", "Error", err)
		return err
	}

	logger.Info("Completing workflow")
	return nil
}
