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

// SetMachineMaintenance is a workflow to set Machine maintenance mode using SetMaintenanceOnSite activity
func SetMachineMaintenance(ctx workflow.Context, request *corev1.MaintenanceRequest) error {
	logger := log.With().Str("Workflow", "SetMachineMaintenance").Logger()

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

	// Invoke SetMachineMaintenanceOnSite activity
	var machineManager activity.ManageMachine

	err := workflow.ExecuteActivity(ctx, machineManager.SetMachineMaintenanceOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "SetMachineMaintenanceOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

func UpdateMachineMetadata(ctx workflow.Context, request *corev1.MachineMetadataUpdateRequest) error {
	logger := log.With().Str("Workflow", "UpdateMachineMetadata").Logger()

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

	// Invoke UpdateMachineMetadataOnSite activity
	var machineManager activity.ManageMachine

	err := workflow.ExecuteActivity(ctx, machineManager.UpdateMachineMetadataOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateMachineMetadataOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// CreateMachineHealthReport inserts the tenant-reported OnlineRepair health report on Site.
func CreateMachineHealthReport(ctx workflow.Context, request *corev1.InsertMachineHealthReportRequest) error {
	logger := log.With().Str("Workflow", "CreateMachineHealthReport").Logger()
	logger.Info().Msg("Starting workflow")

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

	var machineManager activity.ManageMachine
	if err := workflow.ExecuteActivity(ctx, machineManager.CreateMachineHealthReportOnSite, request).Get(ctx, nil); err != nil {
		logger.Error().Err(err).Str("Activity", "CreateMachineHealthReportOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")
	return nil
}

// DeleteMachineHealthReport removes the tenant-reported OnlineRepair health report on Site.
func DeleteMachineHealthReport(ctx workflow.Context, request *corev1.RemoveMachineHealthReportRequest) error {
	logger := log.With().Str("Workflow", "DeleteMachineHealthReport").Logger()
	logger.Info().Msg("Starting workflow")

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

	var machineManager activity.ManageMachine
	if err := workflow.ExecuteActivity(ctx, machineManager.DeleteMachineHealthReportOnSite, request).Get(ctx, nil); err != nil {
		logger.Error().Err(err).Str("Activity", "DeleteMachineHealthReportOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")
	return nil
}

func CollectAndPublishMachineInventory(ctx workflow.Context) error {
	logger := log.With().Str("Workflow", "CollectAndPublishMachineInventory").Logger()

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

	// Invoke CollectAndPublishMachineInventory activity
	var machineInventoryManager activity.ManageMachineInventory

	err := workflow.ExecuteActivity(ctx, machineInventoryManager.CollectAndPublishMachineInventory).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "CollectAndPublishMachineInventory").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// GetDpuMachines is a workflow to retrieve DPU Machines by IDs with network configuration
func GetDpuMachines(ctx workflow.Context, dpuMachineIDs []string) (*corev1.DpuMachineList, error) {
	logger := log.With().Str("Workflow", "GetDpuMachines").Logger()

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

	// Invoke GetDpuMachinesByIDs activity
	var machineManager activity.ManageMachine

	var result corev1.DpuMachineList
	err := workflow.ExecuteActivity(ctx, machineManager.GetDpuMachinesByIDs, dpuMachineIDs).Get(ctx, &result)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "GetDpuMachinesByIDs").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("Completing workflow")

	return &result, nil
}
