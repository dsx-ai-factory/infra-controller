// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

const (
	removeCreateExpectedPowerShelfOnFlowChangeID = "remove-create-expected-power-shelf-on-flow"
	removeCreateExpectedPowerShelfOnFlowVersion  = workflow.Version(1)
)

// DiscoverExpectedPowerShelfInventory is a workflow to fetch Expected Power Shelf inventory on Site and publish to Cloud
func DiscoverExpectedPowerShelfInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverExpectedPowerShelfInventory").Logger()

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
	var inventoryManager activity.ManageExpectedPowerShelfInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverExpectedPowerShelfInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverExpectedPowerShelfInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateExpectedPowerShelf is a workflow to create a new Expected Power Shelf using the CreateExpectedPowerShelfOnSite activity.
func CreateExpectedPowerShelf(ctx workflow.Context, request *corev1.ExpectedPowerShelf) error {
	logger := log.With().Str("Workflow", "ExpectedPowerShelf").Str("Action", "Create").Str("ID", request.GetExpectedPowerShelfId().GetValue()).Str("Expected MAC address", request.BmcMacAddress).Str("Serial", request.ShelfSerialNumber).Logger()

	logger.Info().Msg("starting workflow")

	// No automatic retries: the on-site call is a non-idempotent mutation, and a
	// second attempt gets a fresh activity budget that can outlive both the workflow
	// and the caller. The caller decides whether to retry.
	retrypolicy := &temporal.RetryPolicy{
		MaximumAttempts: 1,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: cloudutils.ActivityStartToCloseTimeout,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var expectedPowerShelfManager activity.ManageExpectedPowerShelf

	// Write to Core first
	err := workflow.ExecuteActivity(ctx, expectedPowerShelfManager.CreateExpectedPowerShelfOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateExpectedPowerShelfOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	// Preserve the Flow activity command when replaying histories created before
	// direct Flow writes were removed.
	if workflow.GetVersion(ctx, removeCreateExpectedPowerShelfOnFlowChangeID, workflow.DefaultVersion, removeCreateExpectedPowerShelfOnFlowVersion) == workflow.DefaultVersion {
		err = workflow.ExecuteActivity(ctx, expectedPowerShelfManager.CreateExpectedPowerShelfOnFlow, request).Get(ctx, nil)
		if err != nil {
			logger.Warn().Err(err).Str("Activity", "CreateExpectedPowerShelfOnFlow").Msg("Failed to create component on Flow, Core write succeeded")
		}
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateExpectedPowerShelf is a workflow to update an Expected Power Shelf using the UpdateExpectedPowerShelfOnSite activity
func UpdateExpectedPowerShelf(ctx workflow.Context, request *corev1.ExpectedPowerShelf) error {
	logger := log.With().Str("Workflow", "ExpectedPowerShelf").Str("Action", "Update").Str("ID", request.GetExpectedPowerShelfId().GetValue()).Str("Expected MAC address", request.BmcMacAddress).Str("Serial", request.ShelfSerialNumber).Logger()

	logger.Info().Msg("starting workflow")

	// No automatic retries: the on-site call is a non-idempotent mutation, and a
	// second attempt gets a fresh activity budget that can outlive both the workflow
	// and the caller. The caller decides whether to retry.
	retrypolicy := &temporal.RetryPolicy{
		MaximumAttempts: 1,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: cloudutils.ActivityStartToCloseTimeout,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var expectedPowerShelfManager activity.ManageExpectedPowerShelf

	err := workflow.ExecuteActivity(ctx, expectedPowerShelfManager.UpdateExpectedPowerShelfOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateExpectedPowerShelfOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteExpectedPowerShelf is a workflow to Delete an Expected Power Shelf using the DeleteExpectedPowerShelfOnSite activity
func DeleteExpectedPowerShelf(ctx workflow.Context, request *corev1.ExpectedPowerShelfRequest) error {
	logger := log.With().Str("Workflow", "ExpectedPowerShelf").Str("Action", "Delete").Str("ID", request.GetExpectedPowerShelfId().GetValue()).Str("optional MAC address", request.BmcMacAddress).Logger()

	logger.Info().Msg("starting workflow")

	// No automatic retries: the on-site call is a non-idempotent mutation, and a
	// second attempt gets a fresh activity budget that can outlive both the workflow
	// and the caller. The caller decides whether to retry.
	retrypolicy := &temporal.RetryPolicy{
		MaximumAttempts: 1,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: cloudutils.ActivityStartToCloseTimeout,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var expectedPowerShelfManager activity.ManageExpectedPowerShelf

	err := workflow.ExecuteActivity(ctx, expectedPowerShelfManager.DeleteExpectedPowerShelfOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteExpectedPowerShelfOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
