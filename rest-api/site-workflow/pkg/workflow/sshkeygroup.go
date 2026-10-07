// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func DiscoverSSHKeyGroupInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverSSHKeyGroupInventory").Logger()

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
	var inventoryManager activity.ManageSSHKeyGroupInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverSSHKeyGroupInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverSSHKeyGroupInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateSSHKeyGroupV2 is a workflow to create new SSH Key Groups using the CreateSSHKeyGroupOnSite activity
// V1 (CreateSSHKeyGroup) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func CreateSSHKeyGroupV2(ctx workflow.Context, request *corev1.CreateTenantKeysetRequest) error {
	logger := log.With().Str("Workflow", "SSHKeyGroup").Str("Action", "Create").Str("SSHKeyGroup ID", request.GetKeysetIdentifier().GetKeysetId()).Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    1 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		MaximumAttempts:    2,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 2 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var sshKeyGroupManager activity.ManageSSHKeyGroup

	err := workflow.ExecuteActivity(ctx, sshKeyGroupManager.CreateSSHKeyGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateSSHKeyGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateSSHKeyGroupV2 is a workflow to update SSH Key Groups using the UpdateSSHKeyGroupOnSite activity
func UpdateSSHKeyGroupV2(ctx workflow.Context, request *corev1.UpdateTenantKeysetRequest) error {
	logger := log.With().Str("Workflow", "SSHKeyGroup").Str("Action", "Update").Str("SSHKeyGroup ID", request.GetKeysetIdentifier().GetKeysetId()).Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    1 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		MaximumAttempts:    2,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 2 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var sshKeyGroupManager activity.ManageSSHKeyGroup

	err := workflow.ExecuteActivity(ctx, sshKeyGroupManager.UpdateSSHKeyGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateSSHKeyGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteSSHKeyGroupV2 is a workflow to Delete SSH Key Groups using the DeleteSSHKeyGroupOnSite activity
// V1 (DeleteSSHKeyGroup) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func DeleteSSHKeyGroupV2(ctx workflow.Context, request *corev1.DeleteTenantKeysetRequest) error {
	logger := log.With().Str("Workflow", "SSHKeyGroup").Str("Action", "Delete").Str("SSHKeyGroup ID", request.GetKeysetIdentifier().GetKeysetId()).Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    1 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		MaximumAttempts:    2,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 2 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var sshKeyGroupManager activity.ManageSSHKeyGroup

	err := workflow.ExecuteActivity(ctx, sshKeyGroupManager.DeleteSSHKeyGroupOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteSSHKeyGroupOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
