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
	removeCreateExpectedSwitchOnFlowChangeID = "remove-create-expected-switch-on-flow"
	removeCreateExpectedSwitchOnFlowVersion  = workflow.Version(1)
)

// DiscoverExpectedSwitchInventory is a workflow to fetch Expected Switch inventory on Site and publish to Cloud
func DiscoverExpectedSwitchInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverExpectedSwitchInventory").Logger()

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
	var inventoryManager activity.ManageExpectedSwitchInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverExpectedSwitchInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverExpectedSwitchInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateExpectedSwitch is a workflow to create a new Expected Switch using the CreateExpectedSwitchOnSite activity.
func CreateExpectedSwitch(ctx workflow.Context, request *corev1.ExpectedSwitch) error {
	logger := log.With().Str("Workflow", "ExpectedSwitch").Str("Action", "Create").Str("ID", request.GetExpectedSwitchId().GetValue()).Str("Expected MAC address", request.BmcMacAddress).Str("Serial", request.SwitchSerialNumber).Logger()

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

	var expectedSwitchManager activity.ManageExpectedSwitch

	// Write to Core first
	err := workflow.ExecuteActivity(ctx, expectedSwitchManager.CreateExpectedSwitchOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateExpectedSwitchOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	// Preserve the Flow activity command when replaying histories created before
	// direct Flow writes were removed.
	if workflow.GetVersion(ctx, removeCreateExpectedSwitchOnFlowChangeID, workflow.DefaultVersion, removeCreateExpectedSwitchOnFlowVersion) == workflow.DefaultVersion {
		err = workflow.ExecuteActivity(ctx, expectedSwitchManager.CreateExpectedSwitchOnFlow, request).Get(ctx, nil)
		if err != nil {
			logger.Warn().Err(err).Str("Activity", "CreateExpectedSwitchOnFlow").Msg("Failed to create component on Flow, Core write succeeded")
		}
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateExpectedSwitch is a workflow to update an Expected Switch using the UpdateExpectedSwitchOnSite activity
func UpdateExpectedSwitch(ctx workflow.Context, request *corev1.ExpectedSwitch) error {
	logger := log.With().Str("Workflow", "ExpectedSwitch").Str("Action", "Update").Str("ID", request.GetExpectedSwitchId().GetValue()).Str("Expected MAC address", request.BmcMacAddress).Str("Serial", request.SwitchSerialNumber).Logger()

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

	var expectedSwitchManager activity.ManageExpectedSwitch

	err := workflow.ExecuteActivity(ctx, expectedSwitchManager.UpdateExpectedSwitchOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateExpectedSwitchOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteExpectedSwitch is a workflow to Delete an Expected Switch using the DeleteExpectedSwitchOnSite activity
func DeleteExpectedSwitch(ctx workflow.Context, request *corev1.ExpectedSwitchRequest) error {
	logger := log.With().Str("Workflow", "ExpectedSwitch").Str("Action", "Delete").Str("ID", request.GetExpectedSwitchId().GetValue()).Str("optional MAC address", request.BmcMacAddress).Logger()

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

	var expectedSwitchManager activity.ManageExpectedSwitch

	err := workflow.ExecuteActivity(ctx, expectedSwitchManager.DeleteExpectedSwitchOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteExpectedSwitchOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
