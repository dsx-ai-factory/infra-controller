// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// CreateOsImage is a workflow to create an OsImage using CreateOsImageOnSite activity
func CreateOsImage(ctx workflow.Context, request *corev1.OsImageAttributes) error {
	logger := log.With().Str("Workflow", "CreateOsImage").Logger()

	logger.Info().Msg("Starting workflow")

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

	// Invoke CreateOsImageOnSite activity
	var osManager activity.ManageOperatingSystem

	err := workflow.ExecuteActivity(ctx, osManager.CreateOsImageOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateOsImageOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// UpdateOsImage is a workflow to update an OsImage using UpdateOsImageOnSite activity
func UpdateOsImage(ctx workflow.Context, request *corev1.OsImageAttributes) error {
	logger := log.With().Str("Workflow", "UpdateOsImage").Logger()

	logger.Info().Msg("Starting workflow")

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

	// Invoke UpdateOsImageOnSite activity
	var osManager activity.ManageOperatingSystem

	err := workflow.ExecuteActivity(ctx, osManager.UpdateOsImageOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateOsImageOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// DeleteOsImage is a workflow to delete an OsImage using DeleteOsImageOnSite activity
func DeleteOsImage(ctx workflow.Context, request *corev1.DeleteOsImageRequest) error {
	logger := log.With().Str("Workflow", "DeleteOsImage").Logger()

	logger.Info().Msg("Starting workflow")

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

	// Invoke DeleteOsImageOnSite activity
	var osManager activity.ManageOperatingSystem

	err := workflow.ExecuteActivity(ctx, osManager.DeleteOsImageOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteOsImageOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func DiscoverOsImageInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverOsImageInventory").Logger()

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
	var inventoryManager activity.ManageOsImageInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverOsImageInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverOsImageInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// DiscoverOperatingSystemInventory triggers Operating System (iPXE / Templated iPXE
// definition) inventory collection from nico-core and publishes it to the cloud for
// reconciliation with the operating_system table.
func DiscoverOperatingSystemInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverOperatingSystemInventory").Logger()
	logger.Info().Msg("Starting workflow")

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

	var inventoryManager activity.ManageOperatingSystemInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverOperatingSystemInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverOperatingSystemInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")
	return nil
}
