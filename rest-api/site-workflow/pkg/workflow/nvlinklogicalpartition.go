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

func DiscoverNVLinkLogicalPartitionInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverNVLinkLogicalPartitionInventory").Logger()

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
	var inventoryManager activity.ManageNVLinkLogicalPartitionInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverNVLinkLogicalPartitionInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverNVLinkLogicalPartitionInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateNVLinkLogicalPartition is a workflow to create new NVLinkLogical Partitions using the CreateNVLinkLogicalPartitionOnSite activity
func CreateNVLinkLogicalPartition(ctx workflow.Context, request *corev1.NVLinkLogicalPartitionCreationRequest) (*corev1.NVLinkLogicalPartition, error) {
	var requestId string
	if request != nil {
		requestId = request.Id.Value
	}

	logger := log.With().Str("Workflow", "NVLinkLogicalPartition").Str("Action", "Create").Str("NVLink Logical Partition ID", requestId).Logger()

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

	var nvLinkPartitionManager activity.ManageNVLinkLogicalPartition

	var nvLinkLogicalPartition corev1.NVLinkLogicalPartition
	err := workflow.ExecuteActivity(ctx, nvLinkPartitionManager.CreateNVLinkLogicalPartitionOnSite, request).Get(ctx, &nvLinkLogicalPartition)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateNVLinkLogicalPartitionOnSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("completing workflow")

	return &nvLinkLogicalPartition, nil
}

// UpdateNVLinkLogicalPartition is a workflow to update NVLinkLogical Partitions using the UpdateNVLinkLogicalPartitionOnSite activity
func UpdateNVLinkLogicalPartition(ctx workflow.Context, request *corev1.NVLinkLogicalPartitionUpdateRequest) error {
	var requestId string
	if request != nil {
		requestId = request.Id.Value
	}

	logger := log.With().Str("Workflow", "NVLinkLogicalPartition").Str("Action", "Update").Str("NVLink Logical Partition ID", requestId).Logger()

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

	var nvLinkPartitionManager activity.ManageNVLinkLogicalPartition

	err := workflow.ExecuteActivity(ctx, nvLinkPartitionManager.UpdateNVLinkLogicalPartitionOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateNVLinkLogicalPartitionOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteNVLinkLogicalPartition is a workflow to Delete NVLinkLogical Partitions using the DeleteNVLinkLogicalPartitionOnSite activity
func DeleteNVLinkLogicalPartition(ctx workflow.Context, request *corev1.NVLinkLogicalPartitionDeletionRequest) error {
	var requestId string
	if request != nil {
		requestId = request.Id.Value
	}

	logger := log.With().Str("Workflow", "NVLinkLogicalPartition").Str("Action", "Delete").Str("NVLink Logical Partition ID", requestId).Logger()

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

	var nvLinkPartitionManager activity.ManageNVLinkLogicalPartition

	err := workflow.ExecuteActivity(ctx, nvLinkPartitionManager.DeleteNVLinkLogicalPartitionOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteNVLinkLogicalPartitionOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
