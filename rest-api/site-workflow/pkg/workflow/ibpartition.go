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

func DiscoverInfiniBandPartitionInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverInfiniBandPartitionInventory").Logger()

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
	var inventoryManager activity.ManageInfiniBandPartitionInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverInfiniBandPartitionInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverInfiniBandPartitionInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateInfiniBandPartitionV2 is a workflow to create new InfiniBand Partitions using the CreateInfiniBandPartitionOnSite activity
// V1 (CreateInfiniBandPartition) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func CreateInfiniBandPartitionV2(ctx workflow.Context, request *corev1.IBPartitionCreationRequest) error {
	logger := log.With().Str("Workflow", "InfiniBandPartition").Str("Action", "Create").Str("IB Partition ID", request.GetId().GetValue()).Str("Name", request.GetConfig().GetName()).Logger()

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

	var ibPartitionManager activity.ManageInfiniBandPartition

	err := workflow.ExecuteActivity(ctx, ibPartitionManager.CreateInfiniBandPartitionOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateInfiniBandPartitionOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateInfiniBandPartition updates an InfiniBand Partition on site using the UpdateInfiniBandPartitionOnSite activity
func UpdateInfiniBandPartition(ctx workflow.Context, request *corev1.IBPartitionUpdateRequest) error {
	logger := log.With().Str("Workflow", "InfiniBandPartition").Str("Action", "Update").Str("IB Partition ID", request.GetId().GetValue()).Logger()

	logger.Info().Msg("starting workflow")

	// No automatic retries: the on-site call is a non-idempotent mutation, and a
	// second attempt gets a fresh activity budget that can outlive both the workflow
	// and the caller. The caller decides whether to retry.
	retrypolicy := &temporal.RetryPolicy{
		MaximumAttempts: 1,
	}
	options := workflow.ActivityOptions{
		StartToCloseTimeout: cloudutils.ActivityStartToCloseTimeout,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var ibPartitionManager activity.ManageInfiniBandPartition

	err := workflow.ExecuteActivity(ctx, ibPartitionManager.UpdateInfiniBandPartitionOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateInfiniBandPartitionOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteInfiniBandPartitionV2 is a workflow to Delete InfiniBand Partitions using the DeleteInfiniBandPartitionOnSite activity
// V1 (DeleteInfiniBandPartition) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func DeleteInfiniBandPartitionV2(ctx workflow.Context, request *corev1.IBPartitionDeletionRequest) error {
	logger := log.With().Str("Workflow", "InfiniBandPartition").Str("Action", "Delete").Str("IB Partition ID", request.GetId().GetValue()).Logger()

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

	var ibPartitionManager activity.ManageInfiniBandPartition

	err := workflow.ExecuteActivity(ctx, ibPartitionManager.DeleteInfiniBandPartitionOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteInfiniBandPartitionOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
