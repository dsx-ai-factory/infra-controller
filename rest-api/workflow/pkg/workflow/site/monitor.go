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

// MonitorHealthForAllSites is a Temporal cron workflow to periodically checks if Site inventory has been received from Site Agent
// TODO: Once health check is available across all Sites, retire this workflow
func MonitorHealthForAllSites(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "Site").Str("Action", "MonitorHealthAll").Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    3 * time.Minute,
		MaximumAttempts:    15,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 3 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var siteManager siteActivity.ManageSite

	err := workflow.ExecuteActivity(ctx, siteManager.MonitorInventoryReceiptForAllSites).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: MonitorInventoryReceiptForAllSites")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteMonitorHealthForAllSitesWorkflow is a helper function to trigger execution of MonitorHealthForAllSites workflow
func ExecuteMonitorHealthForAllSitesWorkflow(ctx context.Context, tc client.Client) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:           "site-monitor-health-all",
		CronSchedule: "@every 3m",
		TaskQueue:    queue.CloudTaskQueue,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, MonitorHealthForAllSites)

	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: MonitorHealthForAllSites")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}

// MonitorSiteTemporalNamespaces is a Temporal cron workflow to periodically check and delete orphaned Temporal namespaces
func MonitorSiteTemporalNamespaces(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "Site").Str("Action", "MonitorSiteTemporalNamespaces").Logger()

	logger.Info().Msg("starting workflow")

	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    1 * time.Second,
		BackoffCoefficient: 1.0,
		MaximumInterval:    2 * time.Minute,
		MaximumAttempts:    2,
	}

	options := workflow.ActivityOptions{
		StartToCloseTimeout: 30 * time.Minute,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var siteManager siteActivity.ManageSite

	// Execute activity to get temporal namespaces
	err := workflow.ExecuteActivity(ctx, siteManager.DeleteOrphanedSiteTemporalNamespaces).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to execute activity: DeleteOrphanedSiteTemporalNamespaces")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteMonitorSiteTemporalNamespaces is a helper function to trigger execution of MonitorSiteTemporalNamespaces workflow
func ExecuteMonitorSiteTemporalNamespaces(ctx context.Context, tc client.Client) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:           "monitor-site-temporal-namespaces",
		CronSchedule: "@every 1h", // Run hourly
		TaskQueue:    queue.CloudTaskQueue,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, MonitorSiteTemporalNamespaces)
	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: MonitorSiteTemporalNamespaces")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}

// MonitorTemporalCertExpirationForAllSites is a Temporal cron workflow to periodically rotate certs and OTPs
func MonitorTemporalCertExpirationForAllSites(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "MonitorTemporalCertExpirationForAllSites").Logger()
	logger.Info().Msg("Starting workflow")

	// A retry, including one after a timeout, would restart the batch and roll the OTP again for Sites
	// already rotated. So the activity gets a single attempt with room for a large fleet, and the next
	// cron run picks up whatever this one missed.
	options := workflow.ActivityOptions{
		StartToCloseTimeout: 30 * time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 1},
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	// Execute the activity
	err := workflow.ExecuteActivity(ctx, siteActivity.ManageSite.CheckOTPExpirationAndRenewForAllSites).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: CheckOTPExpirationAndRenewForAllSites")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteMonitorTemporalCertExpirationForAllSites is a helper function to trigger execution of MonitorTemporalCertExpirationForAllSites
func ExecuteMonitorTemporalCertExpirationForAllSites(ctx context.Context, tc client.Client) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:           "rotate-certs-and-otps",
		CronSchedule: "@every 24h", // Run daily
		TaskQueue:    queue.CloudTaskQueue,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, MonitorTemporalCertExpirationForAllSites)
	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: MonitorTemporalCertExpirationForAllSites")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}

// UpdateAgentCertExpiry updates the AgentCertExpiry field for a site
func UpdateAgentCertExpiry(ctx workflow.Context, siteIDStr string, certExpiry time.Time) error {
	logger := log.With().Str("Workflow", "UpdateAgentCertExpiry").Str("SiteID", siteIDStr).Logger()
	logger.Info().Msg("Starting workflow to update AgentCertExpiry")

	// Parse siteID
	siteID, err := uuid.Parse(siteIDStr)
	if err != nil {
		logger.Error().Err(err).Msg("Invalid siteID")
		return err
	}

	// Set up activity options
	options := workflow.ActivityOptions{
		StartToCloseTimeout: 5 * time.Minute,
		RetryPolicy: &temporal.RetryPolicy{
			InitialInterval:    1 * time.Second,
			BackoffCoefficient: 2.0,
			MaximumInterval:    1 * time.Minute,
			MaximumAttempts:    3,
		},
	}
	ctx = workflow.WithActivityOptions(ctx, options)

	// Create an instance of ManageSite activities
	var manageSite siteActivity.ManageSite

	// Execute the activity
	err = workflow.ExecuteActivity(ctx, manageSite.UpdateAgentCertExpiry, siteID, certExpiry).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to execute UpdateAgentCertExpiry activity")
		return err
	}

	logger.Info().Msg("Workflow completed successfully")
	return nil
}
