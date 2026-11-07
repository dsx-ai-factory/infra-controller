// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	"go.temporal.io/sdk/client"

	siteActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// DeleteSiteComponents is a Temporal workflow to initiate delete workflow if exists Instance/InstanceType/Machine/Subnet/VPC via Site Agent
func DeleteSiteComponents(ctx workflow.Context, siteID uuid.UUID, infrastructureProviderID uuid.UUID, purgeMachines bool) error {
	logger := log.With().Str("Workflow", "Site").Str("Action", "Delete").Str("Site ID", siteID.String()).Str("InfrastructureProviderID ID", infrastructureProviderID.String()).Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    2 * time.Minute,
		MaximumAttempts:    15,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 2 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var siteManager siteActivity.ManageSite

	err := workflow.ExecuteActivity(ctx, siteManager.DeleteSiteComponentsFromDB, siteID, infrastructureProviderID, purgeMachines).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: DeleteSiteComponentsFromDB")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteDeleteSiteComponentsWorkflow is a helper function to trigger execution of delete Site Components workflow
func ExecuteDeleteSiteComponentsWorkflow(ctx context.Context, tc client.Client, siteID uuid.UUID, infrastructureProviderID uuid.UUID, purgeMachines bool) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:        "site-delete-component-" + siteID.String(),
		TaskQueue: queue.CloudTaskQueue,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, DeleteSiteComponents, siteID, infrastructureProviderID, purgeMachines)

	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: DeleteSiteComponents")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}
