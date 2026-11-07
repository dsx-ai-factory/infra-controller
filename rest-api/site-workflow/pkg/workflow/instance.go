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

// UpdateInstance is a workflow to update Instance data using then UpdateInstanceOnSite activity
func UpdateInstance(ctx workflow.Context, updateRequest *corev1.InstanceConfigUpdateRequest) error {
	logger := log.With().Str("Workflow", "Instance").Str("Action", "Update").Str("Instance ID", updateRequest.InstanceId.String()).Logger()

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

	var instanceManager activity.ManageInstance

	err := workflow.ExecuteActivity(ctx, instanceManager.UpdateInstanceOnSite, updateRequest).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateInstanceOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateInstanceV2 is a workflow to create (allocate) new Instances using the CreateInstanceOnSite activity
// V1 (CreateInstance) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func CreateInstanceV2(ctx workflow.Context, request *corev1.InstanceAllocationRequest) error {
	logger := log.With().Str("Workflow", "Instance").Str("Action", "Create").Str("Machine ID", request.MachineId.Id).Logger()

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

	var instanceManager activity.ManageInstance

	err := workflow.ExecuteActivity(ctx, instanceManager.CreateInstanceOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateInstanceOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateInstances is a workflow to create (allocate) multiple Instances in a single transaction
// using the CreateInstancesOnSite activity.
func CreateInstances(ctx workflow.Context, request *corev1.BatchInstanceAllocationRequest) error {
	logger := log.With().
		Str("Workflow", "Instance").
		Str("Action", "CreateInstances").
		Int("Count", len(request.InstanceRequests)).
		Logger()

	logger.Info().Msg("Starting batch instance allocation workflow")

	// No automatic retries: the on-site call is a non-idempotent mutation, and a
	// second attempt gets a fresh activity budget that can outlive both the workflow
	// and the caller. The caller decides whether to retry.
	retrypolicy := &temporal.RetryPolicy{
		MaximumAttempts: 1,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		// A batch takes longer on Site than a single allocation. It still shares the
		// ladder, because the REST caller waits no longer for a batch. A batch that
		// cannot finish inside the budget needs an async contract, not a budget that
		// outlives its caller.
		StartToCloseTimeout: cloudutils.ActivityStartToCloseTimeout,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var instanceManager activity.ManageInstance

	err := workflow.ExecuteActivity(ctx, instanceManager.CreateInstancesOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CreateInstancesOnSite").Msg("Failed to execute batch creation activity from workflow")
		return err
	}

	logger.Info().Int("Count", len(request.InstanceRequests)).Msg("Completing batch instance allocation workflow")

	return nil
}

// DeleteInstanceV2 is a workflow to delete new Instances using the DeleteInstanceOnSite activity
// V1 (DeleteInstance) is found in cloud-workflow and uses a different activity that does not speak
// to nico directly.
func DeleteInstanceV2(ctx workflow.Context, request *corev1.InstanceReleaseRequest) error {

	logger := log.With().Str("Workflow", "Instance").Str("Action", "Delete").Str("Request", request.String()).Logger()

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

	var instanceManager activity.ManageInstance

	err := workflow.ExecuteActivity(ctx, instanceManager.DeleteInstanceOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteInstanceOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// RebootInstance is a workflow to reboot Instances using the RebootInstanceOnSite activity
func RebootInstance(ctx workflow.Context, request *corev1.InstancePowerRequest) error {
	logger := log.With().Str("Workflow", "Instance").Str("Action", "Reboot").Str("Instance ID", request.InstanceId.Value).Logger()

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

	var instanceManager activity.ManageInstance

	err := workflow.ExecuteActivity(ctx, instanceManager.RebootInstanceOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "RebootInstanceOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func DiscoverInstanceInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "DiscoverInstanceInventory").Logger()

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

	// Invoke DiscoverInstanceInventory activity
	var instanceInventoryManager activity.ManageInstanceInventory

	err := workflow.ExecuteActivity(ctx, instanceInventoryManager.DiscoverInstanceInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "DiscoverInstanceInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
