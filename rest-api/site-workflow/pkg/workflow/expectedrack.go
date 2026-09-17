// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

const (
	removeCreateExpectedRackOnFlowChangeID = "remove-create-expected-rack-on-flow"
	removeCreateExpectedRackOnFlowVersion  = workflow.Version(1)
)

// expectedRackActivityOptions returns the common ActivityOptions used by all
// ExpectedRack workflows.
func expectedRackActivityOptions() workflow.ActivityOptions {
	// No automatic retries: the on-site call is a non-idempotent mutation, and a
	// second attempt gets a fresh activity budget that can outlive both the workflow
	// and the caller. The caller decides whether to retry.
	retrypolicy := &temporal.RetryPolicy{
		MaximumAttempts: 1,
	}
	return workflow.ActivityOptions{
		StartToCloseTimeout: cloudutils.ActivityStartToCloseTimeout,
		RetryPolicy:         retrypolicy,
	}
}

// DiscoverExpectedRackInventory is a workflow to fetch Expected Rack inventory on Site and publish to Cloud
func DiscoverExpectedRackInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverExpectedRackInventory").Logger()

	logger.Info().Msg("Starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		// This is executed every 3 minutes, so we don't want too many retry attempts
		MaximumAttempts: 2,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 2 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	// Invoke activity
	var inventoryManager activity.ManageExpectedRackInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverExpectedRackInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverExpectedRackInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateExpectedRack is a workflow to create a new Expected Rack using the
// CreateExpectedRackOnSite activity.
func CreateExpectedRack(ctx workflow.Context, request *corev1.ExpectedRack) error {
	logger := log.With().Str("Workflow", "ExpectedRack").Str("Action", "Create").Str("ID", request.GetRackId().GetId()).Str("RackProfileID", request.GetRackProfileId().GetId()).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackActivityOptions())

	var expectedRackManager activity.ManageExpectedRack

	// Write to Core first
	err := workflow.ExecuteActivity(ctx, expectedRackManager.CreateExpectedRackOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateExpectedRackOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	// Preserve the Flow activity command when replaying histories created before
	// direct Flow writes were removed.
	if workflow.GetVersion(ctx, removeCreateExpectedRackOnFlowChangeID, workflow.DefaultVersion, removeCreateExpectedRackOnFlowVersion) == workflow.DefaultVersion {
		err = workflow.ExecuteActivity(ctx, expectedRackManager.CreateExpectedRackOnFlow, request).Get(ctx, nil)
		if err != nil {
			logger.Warn().Err(err).Str("Activity", "CreateExpectedRackOnFlow").Msg("Failed to create rack on Flow, Core write succeeded")
		}
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateExpectedRack is a workflow to update an Expected Rack using the
// UpdateExpectedRackOnSite activity.
func UpdateExpectedRack(ctx workflow.Context, request *corev1.ExpectedRack) error {
	logger := log.With().Str("Workflow", "ExpectedRack").Str("Action", "Update").Str("ID", request.GetRackId().GetId()).Str("RackProfileID", request.GetRackProfileId().GetId()).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackActivityOptions())

	var expectedRackManager activity.ManageExpectedRack

	err := workflow.ExecuteActivity(ctx, expectedRackManager.UpdateExpectedRackOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateExpectedRackOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteExpectedRack is a workflow to delete an Expected Rack using the
// DeleteExpectedRackOnSite activity.
func DeleteExpectedRack(ctx workflow.Context, request *corev1.ExpectedRackRequest) error {
	logger := log.With().Str("Workflow", "ExpectedRack").Str("Action", "Delete").Str("ID", request.GetRackId()).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackActivityOptions())

	var expectedRackManager activity.ManageExpectedRack

	err := workflow.ExecuteActivity(ctx, expectedRackManager.DeleteExpectedRackOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteExpectedRackOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ReplaceAllExpectedRacks is a workflow to replace all Expected Racks on Site
// using the ReplaceAllExpectedRacksOnSite activity.
func ReplaceAllExpectedRacks(ctx workflow.Context, request *corev1.ExpectedRackList) error {
	logger := log.With().Str("Workflow", "ExpectedRack").Str("Action", "ReplaceAll").Int("Count", len(request.GetExpectedRacks())).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackActivityOptions())

	var expectedRackManager activity.ManageExpectedRack

	err := workflow.ExecuteActivity(ctx, expectedRackManager.ReplaceAllExpectedRacksOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "ReplaceAllExpectedRacksOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteAllExpectedRacks is a workflow to delete all Expected Racks on Site
// using the DeleteAllExpectedRacksOnSite activity.
func DeleteAllExpectedRacks(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "ExpectedRack").Str("Action", "DeleteAll").Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackActivityOptions())

	var expectedRackManager activity.ManageExpectedRack

	err := workflow.ExecuteActivity(ctx, expectedRackManager.DeleteAllExpectedRacksOnSite).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteAllExpectedRacksOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
