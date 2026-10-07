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

// CreateInstanceType is a workflow to create new InstanceTypes using the CreateInstanceTypeOnSite activity
// to speak to nico directly.
func CreateInstanceType(ctx workflow.Context, request *corev1.CreateInstanceTypeRequest) error {
	logger := log.With().Str("Workflow", "InstanceType").Str("Action", "Create").Str("InstanceType ID", request.GetId()).Logger()

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

	var instanceTypeManager activity.ManageInstanceType

	err := workflow.ExecuteActivity(ctx, instanceTypeManager.CreateInstanceTypeOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateInstanceTypeOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// UpdateInstanceType is a workflow to update InstanceType data using then UpdateInstanceTypeOnSite activity
func UpdateInstanceType(ctx workflow.Context, updateRequest *corev1.UpdateInstanceTypeRequest) error {
	logger := log.With().Str("Workflow", "InstanceType").Str("Action", "Update").Str("InstanceType ID", updateRequest.GetId()).Logger()

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

	var instanceTypeManager activity.ManageInstanceType

	err := workflow.ExecuteActivity(ctx, instanceTypeManager.UpdateInstanceTypeOnSite, updateRequest).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateInstanceTypeOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// DeleteInstanceType is a workflow to delete new InstanceTypes using the DeleteInstanceTypeOnSite activity
func DeleteInstanceType(ctx workflow.Context, request *corev1.DeleteInstanceTypeRequest) error {

	logger := log.With().Str("Workflow", "InstanceType").Str("Action", "Delete").Str("Request", request.String()).Logger()

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

	var instanceTypeManager activity.ManageInstanceType

	err := workflow.ExecuteActivity(ctx, instanceTypeManager.DeleteInstanceTypeOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteInstanceTypeOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// AssociateMachinesWithInstanceType is a workflow to associate machines with an InstanceType
func AssociateMachinesWithInstanceType(ctx workflow.Context, request *corev1.AssociateMachinesWithInstanceTypeRequest) error {

	logger := log.With().Str("Workflow", "InstanceType").Str("Action", "AssociateMachinesWithInstanceType").Str("Request", request.String()).Logger()

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

	var instanceTypeManager activity.ManageInstanceType

	err := workflow.ExecuteActivity(ctx, instanceTypeManager.AssociateMachinesWithInstanceTypeOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "AssociateMachinesWithInstanceTypeOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// RemoveMachineInstanceTypeAssociation is a workflow to remove the relationship between a Machine and InstanceType
func RemoveMachineInstanceTypeAssociation(ctx workflow.Context, request *corev1.RemoveMachineInstanceTypeAssociationRequest) error {

	logger := log.With().Str("Workflow", "InstanceType").Str("Action", "RemoveMachineInstanceTypeAssociation").Str("Request", request.String()).Logger()

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

	var instanceTypeManager activity.ManageInstanceType

	err := workflow.ExecuteActivity(ctx, instanceTypeManager.RemoveMachineInstanceTypeAssociationOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "RemoveMachineInstanceTypeAssociation").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func DiscoverInstanceTypeInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverInstanceTypeInventory").Logger()

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

	// Invoke DiscoverInstanceTypeInventory activity
	var instanceTypeInventoryManager activity.ManageInstanceTypeInventory

	err := workflow.ExecuteActivity(ctx, instanceTypeInventoryManager.DiscoverInstanceTypeInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverInstanceTypeInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
