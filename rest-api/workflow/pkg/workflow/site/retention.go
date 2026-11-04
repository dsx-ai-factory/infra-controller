// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"time"

	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	siteActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// DeleteSuccessfulInventoryWorkflows is a Temporal cron workflow that deletes successful inventory
// workflows from the Site namespace before the namespace retention would. Failed, canceled,
// terminated, and timed out runs are left to the namespace retention.
func DeleteSuccessfulInventoryWorkflows(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DeleteSuccessfulInventoryWorkflows").Logger()

	logger.Info().Msg("starting workflow")

	options := workflow.ActivityOptions{
		StartToCloseTimeout: time.Minute,
		RetryPolicy: &temporal.RetryPolicy{
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
			MaximumInterval:    time.Minute,
			MaximumAttempts:    3,
		},
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var retentionManager siteActivity.ManageInventoryRetention

	err := workflow.ExecuteActivity(ctx, retentionManager.StartSuccessfulInventoryWorkflowDeletion).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: StartSuccessfulInventoryWorkflowDeletion")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteDeleteSuccessfulInventoryWorkflows is a helper function to trigger execution of
// DeleteSuccessfulInventoryWorkflows workflow. It returns the running cron if one was already started.
func ExecuteDeleteSuccessfulInventoryWorkflows(ctx context.Context, tc client.Client) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:           "delete-successful-inventory-workflows",
		CronSchedule: "@every 1h",
		TaskQueue:    queue.SiteTaskQueue,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, DeleteSuccessfulInventoryWorkflows)
	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: DeleteSuccessfulInventoryWorkflows")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}
