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

// CreateNetworkSecurityGroup is a workflow to create new NetworkSecurityGroups using the CreateNetworkSecurityGroupOnSite activity
// to speak to nico directly.
func CreateNetworkSecurityGroup(ctx workflow.Context, request *corev1.CreateNetworkSecurityGroupRequest) error {
	logger := log.With().Str("Workflow", "NetworkSecurityGroup").Str("Action", "Create").Str("NetworkSecurityGroup ID", request.GetId()).Logger()

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

	var networkSecurityGroupManager activity.ManageNetworkSecurityGroup

	err := workflow.ExecuteActivity(ctx, networkSecurityGroupManager.CreateNetworkSecurityGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateNetworkSecurityGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// UpdateNetworkSecurityGroup is a workflow to update NetworkSecurityGroup data using then UpdateNetworkSecurityGroupOnSite activity
func UpdateNetworkSecurityGroup(ctx workflow.Context, updateRequest *corev1.UpdateNetworkSecurityGroupRequest) error {
	logger := log.With().Str("Workflow", "NetworkSecurityGroup").Str("Action", "Update").Str("NetworkSecurityGroup ID", updateRequest.GetId()).Logger()

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

	var networkSecurityGroupManager activity.ManageNetworkSecurityGroup

	err := workflow.ExecuteActivity(ctx, networkSecurityGroupManager.UpdateNetworkSecurityGroupOnSite, updateRequest).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateNetworkSecurityGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// DeleteNetworkSecurityGroup is a workflow to delete new NetworkSecurityGroups using the DeleteNetworkSecurityGroupOnSite activity
func DeleteNetworkSecurityGroup(ctx workflow.Context, request *corev1.DeleteNetworkSecurityGroupRequest) error {

	logger := log.With().Str("Workflow", "NetworkSecurityGroup").Str("Action", "Delete").Str("Request", request.String()).Logger()

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

	var networkSecurityGroupManager activity.ManageNetworkSecurityGroup

	err := workflow.ExecuteActivity(ctx, networkSecurityGroupManager.DeleteNetworkSecurityGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteNetworkSecurityGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func DiscoverNetworkSecurityGroupInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverNetworkSecurityGroupInventory").Logger()

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

	// Invoke DiscoverNetworkSecurityGroupInventory activity
	var networkSecurityGroupInventoryManager activity.ManageNetworkSecurityGroupInventory

	err := workflow.ExecuteActivity(ctx, networkSecurityGroupInventoryManager.DiscoverNetworkSecurityGroupInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverNetworkSecurityGroupInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
