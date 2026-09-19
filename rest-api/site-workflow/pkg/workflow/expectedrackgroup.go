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

// expectedRackGroupActivityOptions returns the common ActivityOptions used by all
// ExpectedRackGroup workflows.
func expectedRackGroupActivityOptions() workflow.ActivityOptions {
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

// DiscoverExpectedRackGroupInventory is a workflow to fetch Expected Rack Group inventory on Site and publish to Cloud
func DiscoverExpectedRackGroupInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverExpectedRackGroupInventory").Logger()

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
	var inventoryManager activity.ManageExpectedRackGroupInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverExpectedRackGroupInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverExpectedRackGroupInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateExpectedRackGroup is a workflow to create a new Expected Rack Group using the
// CreateExpectedRackGroupOnSite activity.
func CreateExpectedRackGroup(ctx workflow.Context, request *corev1.ExpectedRackGroup) error {
	logger := log.With().Str("Workflow", "ExpectedRackGroup").Str("Action", "Create").Str("ID", request.GetRackGroupId().GetId()).Str("Topology", request.GetTopology()).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackGroupActivityOptions())

	var expectedRackGroupManager activity.ManageExpectedRackGroup

	// Write to Core first
	err := workflow.ExecuteActivity(ctx, expectedRackGroupManager.CreateExpectedRackGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateExpectedRackGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateExpectedRackGroup is a workflow to update an Expected Rack Group using the
// UpdateExpectedRackGroupOnSite activity.
func UpdateExpectedRackGroup(ctx workflow.Context, request *corev1.ExpectedRackGroup) error {
	logger := log.With().Str("Workflow", "ExpectedRackGroup").Str("Action", "Update").Str("ID", request.GetRackGroupId().GetId()).Str("Topology", request.GetTopology()).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackGroupActivityOptions())

	var expectedRackGroupManager activity.ManageExpectedRackGroup

	err := workflow.ExecuteActivity(ctx, expectedRackGroupManager.UpdateExpectedRackGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateExpectedRackGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteExpectedRackGroup is a workflow to delete an Expected Rack Group using the
// DeleteExpectedRackGroupOnSite activity.
func DeleteExpectedRackGroup(ctx workflow.Context, request *corev1.ExpectedRackGroupRequest) error {
	logger := log.With().Str("Workflow", "ExpectedRackGroup").Str("Action", "Delete").Str("ID", request.GetRackGroupId()).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackGroupActivityOptions())

	var expectedRackGroupManager activity.ManageExpectedRackGroup

	err := workflow.ExecuteActivity(ctx, expectedRackGroupManager.DeleteExpectedRackGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteExpectedRackGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ReplaceAllExpectedRackGroups is a workflow to replace all Expected Rack Groups on Site
// using the ReplaceAllExpectedRackGroupsOnSite activity.
func ReplaceAllExpectedRackGroups(ctx workflow.Context, request *corev1.ExpectedRackGroupList) error {
	logger := log.With().Str("Workflow", "ExpectedRackGroup").Str("Action", "ReplaceAll").Int("Count", len(request.GetExpectedRackGroups())).Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackGroupActivityOptions())

	var expectedRackGroupManager activity.ManageExpectedRackGroup

	err := workflow.ExecuteActivity(ctx, expectedRackGroupManager.ReplaceAllExpectedRackGroupsOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "ReplaceAllExpectedRackGroupsOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteAllExpectedRackGroups is a workflow to delete all Expected Rack Groups on Site
// using the DeleteAllExpectedRackGroupsOnSite activity.
func DeleteAllExpectedRackGroups(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "ExpectedRackGroup").Str("Action", "DeleteAll").Logger()

	logger.Info().Msg("starting workflow")

	ctx = workflow.WithActivityOptions(ctx, expectedRackGroupActivityOptions())

	var expectedRackGroupManager activity.ManageExpectedRackGroup

	err := workflow.ExecuteActivity(ctx, expectedRackGroupManager.DeleteAllExpectedRackGroupsOnSite).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteAllExpectedRackGroupsOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
