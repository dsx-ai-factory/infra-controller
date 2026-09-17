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

// CreateVpcPrefix is a workflow to create an VpcPrefix using CreateVpcPrefixOnSite activity
func CreateVpcPrefix(ctx workflow.Context, request *corev1.VpcPrefixCreationRequest) error {
	logger := log.With().Str("Workflow", "CreateVpcPrefix").Logger()

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

	// Invoke CreateVpcPrefixOnSite activity
	var vpcPrefixManager activity.ManageVpcPrefix

	err := workflow.ExecuteActivity(ctx, vpcPrefixManager.CreateVpcPrefixOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateVpcPrefixOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// UpdateVpcPrefix is a workflow to update an VpcPrefix using UpdateVpcPrefixOnSite activity
func UpdateVpcPrefix(ctx workflow.Context, request *corev1.VpcPrefixUpdateRequest) error {
	logger := log.With().Str("Workflow", "UpdateVpcPrefix").Logger()

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

	// Invoke UpdateVpcPrefixOnSite activity
	var vpcPrefixManager activity.ManageVpcPrefix

	err := workflow.ExecuteActivity(ctx, vpcPrefixManager.UpdateVpcPrefixOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateVpcPrefixOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// DeleteVpcPrefix is a workflow to delete an VpcPrefix using DeleteVpcPrefixOnSite activity
func DeleteVpcPrefix(ctx workflow.Context, request *corev1.VpcPrefixDeletionRequest) error {
	logger := log.With().Str("Workflow", "DeleteVpcPrefix").Logger()

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

	// Invoke DeleteVpcPrefixOnSite activity
	var vpcPrefixManager activity.ManageVpcPrefix

	err := workflow.ExecuteActivity(ctx, vpcPrefixManager.DeleteVpcPrefixOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteVpcPrefixOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func DiscoverVpcPrefixInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverVpcPrefixInventory").Logger()

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
	var inventoryManager activity.ManageVpcPrefixInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverVpcPrefixInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverVpcPrefixInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
