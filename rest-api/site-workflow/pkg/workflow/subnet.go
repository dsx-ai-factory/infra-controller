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

// CreateSubnetV2 is a workflow to create new Subnets using the CreateSubnetOnSite activity
// V1 (CreateSubnet) is found cloud-workflow and uses a different activity that does not speak
// to nico directly.
func CreateSubnetV2(ctx workflow.Context, request *corev1.NetworkSegmentCreationRequest) error {
	logger := log.With().Str("Workflow", "Subnet").Str("Action", "Create").Str("VPC ID", request.VpcId.String()).Str("Name", request.Name).Logger()

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

	var subnetManager activity.ManageSubnet

	err := workflow.ExecuteActivity(ctx, subnetManager.CreateSubnetOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateSubnetOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// DeleteSubnetV2 is a workflow to delete Subnets using the DeleteSubnetOnSite activity
// V1 (DeleteSubnet) is found cloud-workflow and uses a different activity that does not speak
// to nico directly.
func DeleteSubnetV2(ctx workflow.Context, request *corev1.NetworkSegmentDeletionRequest) error {
	logger := log.With().Str("Workflow", "Subnet").Str("Action", "Delete").Str("Subnet ID", request.GetId().GetValue()).Logger()

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

	var subnetManager activity.ManageSubnet

	err := workflow.ExecuteActivity(ctx, subnetManager.DeleteSubnetOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteSubnetOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func DiscoverSubnetInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverSubnetInventory").Logger()

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
	var inventoryManager activity.ManageSubnetInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverSubnetInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverSubnetInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
