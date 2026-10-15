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

func DiscoverVPCInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverVPCInventory").Logger()

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
	var inventoryManager activity.ManageVPCInventory

	err := workflow.ExecuteActivity(ctx, inventoryManager.DiscoverVPCInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverVPCInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateVPCV2 is a workflow to create new VPCs using the CreateVpcOnSite activity
// V1 (CreateVPC) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func CreateVPCV2(ctx workflow.Context, request *corev1.VpcCreationRequest) (*corev1.Vpc, error) {
	logger := log.With().Str("Workflow", "VPC").Str("Action", "Create").Str("VPC ID", request.GetId().GetValue()).Str("Name", request.Name).Logger()

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

	var vpcManager activity.ManageVPC
	var controllerVpc corev1.Vpc

	err := workflow.ExecuteActivity(ctx, vpcManager.CreateVpcOnSite, request).Get(ctx, &controllerVpc)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateVpcOnSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("completing workflow")

	return &controllerVpc, nil
}

// UpdateVPC is a workflow to update VPCs using the UpdateVpcOnSite activity
func UpdateVPC(ctx workflow.Context, request *corev1.VpcUpdateRequest) error {
	logger := log.With().Str("Workflow", "VPC").Str("Action", "Update").Str("VPC ID", request.GetId().GetValue()).Logger()

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

	var vpcManager activity.ManageVPC

	err := workflow.ExecuteActivity(ctx, vpcManager.UpdateVpcOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateVpcOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// DeleteVPCV2 is a workflow to Delete VPCs using the DeleteVpcOnSite activity
// V1 (DeleteVPC) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func DeleteVPCV2(ctx workflow.Context, request *corev1.VpcDeletionRequest) error {
	logger := log.With().Str("Workflow", "VPC").Str("Action", "Delete").Str("VPC ID", request.GetId().GetValue()).Logger()

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

	var vpcManager activity.ManageVPC

	err := workflow.ExecuteActivity(ctx, vpcManager.DeleteVpcOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteVpcOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// UpdateVPCVirtualization is a workflow to update VPC virtualization type
func UpdateVPCVirtualization(ctx workflow.Context, request *corev1.VpcUpdateVirtualizationRequest) error {
	logger := log.With().Str("Workflow", "VPC").Str("Action", "Update Virtualization").Str("VPC ID", request.GetId().GetValue()).Logger()

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

	var vpcManager activity.ManageVPC

	err := workflow.ExecuteActivity(ctx, vpcManager.UpdateVpcVirtualizationOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateVpcVirtualizationOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
