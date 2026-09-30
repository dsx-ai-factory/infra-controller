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

// EnableDisableMachineValidationTest is a workflow to enable/disable machine validation test using EnableDisableMachineValidationTestOnSite activity
func EnableDisableMachineValidationTest(ctx workflow.Context, request *corev1.MachineValidationTestEnableDisableTestRequest) error {
	logger := log.With().Str("Workflow", "EnableDisableMachineValidationTest").Logger()

	logger.Info().Msg("Starting workflow")

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	err := workflow.ExecuteActivity(ctx, manager.EnableDisableMachineValidationTestOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "EnableDisableMachineValidationTestOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// PersistValidationResult is a workflow to persist validation result using PersistValidationResultOnSite activity
func PersistValidationResult(ctx workflow.Context, request *corev1.MachineValidationResultPostRequest) error {
	logger := log.With().Str("Workflow", "PersistValidationResult").Logger()

	logger.Info().Msg("Starting workflow")

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	err := workflow.ExecuteActivity(ctx, manager.PersistValidationResultOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "PersistValidationResultOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// GetMachineValidationResults is a workflow to get machine validation results using GetMachineValidationResultsFromSite activity
func GetMachineValidationResults(ctx workflow.Context, request *corev1.MachineValidationGetRequest) (*corev1.MachineValidationResultList, error) {
	logger := log.With().Str("Workflow", "GetMachineValidationResults").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	var response corev1.MachineValidationResultList
	err := workflow.ExecuteActivity(ctx, manager.GetMachineValidationResultsFromSite, request).Get(ctx, &response)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "GetMachineValidationResultsFromSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("Completing workflow")

	return &response, nil
}

// GetMachineValidationRuns is a workflow to get machine validation runs using GetMachineValidationRunsFromSite activity
func GetMachineValidationRuns(ctx workflow.Context, request *corev1.MachineValidationRunListGetRequest) (*corev1.MachineValidationRunList, error) {
	logger := log.With().Str("Workflow", "GetMachineValidationRuns").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	var response corev1.MachineValidationRunList
	err := workflow.ExecuteActivity(ctx, manager.GetMachineValidationRunsFromSite, request).Get(ctx, &response)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "GetMachineValidationRunsFromSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("Completing workflow")

	return &response, nil
}

// GetMachineValidationTests is a workflow to get machine validation tests using GetMachineValidationTestsFromSite activity
func GetMachineValidationTests(ctx workflow.Context, request *corev1.MachineValidationTestsGetRequest) (*corev1.MachineValidationTestsGetResponse, error) {
	logger := log.With().Str("Workflow", "GetMachineValidationTests").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	var response corev1.MachineValidationTestsGetResponse
	err := workflow.ExecuteActivity(ctx, manager.GetMachineValidationTestsFromSite, request).Get(ctx, &response)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "GetMachineValidationTestsFromSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("Completing workflow")

	return &response, nil
}

// AddMachineValidationTest is a workflow to add machine validation test using AddMachineValidationTestOnSite activity
func AddMachineValidationTest(ctx workflow.Context, request *corev1.MachineValidationTestAddRequest) (*corev1.MachineValidationTestAddUpdateResponse, error) {
	logger := log.With().Str("Workflow", "AddMachineValidationTest").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	var response corev1.MachineValidationTestAddUpdateResponse
	err := workflow.ExecuteActivity(ctx, manager.AddMachineValidationTestOnSite, request).Get(ctx, &response)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "AddMachineValidationTestOnSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("Completing workflow")

	return &response, nil
}

// UpdateMachineValidationTest is a workflow to add machine validation test using UpdateMachineValidationTestOnSite activity
func UpdateMachineValidationTest(ctx workflow.Context, request *corev1.MachineValidationTestUpdateRequest) error {
	logger := log.With().Str("Workflow", "UpdateMachineValidationTest").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	err := workflow.ExecuteActivity(ctx, manager.UpdateMachineValidationTestOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "UpdateMachineValidationTestOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// GetMachineValidationExternalConfigs is a workflow to get machine validation tests using GetMachineValidationExternalConfigsFromSite activity
func GetMachineValidationExternalConfigs(ctx workflow.Context, request *corev1.GetMachineValidationExternalConfigsRequest) (*corev1.GetMachineValidationExternalConfigsResponse, error) {
	logger := log.With().Str("Workflow", "GetMachineValidationExternalConfigs").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	var response corev1.GetMachineValidationExternalConfigsResponse
	err := workflow.ExecuteActivity(ctx, manager.GetMachineValidationExternalConfigsFromSite, request).Get(ctx, &response)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "GetMachineValidationExternalConfigsFromSite").Msg("Failed to execute activity from workflow")
		return nil, err
	}

	logger.Info().Msg("Completing workflow")

	return &response, nil
}

// AddUpdateMachineValidationExternalConfig is a workflow to add machine validation test using AddUpdateMachineValidationExternalConfigOnSite activity
func AddUpdateMachineValidationExternalConfig(ctx workflow.Context, request *corev1.AddUpdateMachineValidationExternalConfigRequest) error {
	logger := log.With().Str("Workflow", "AddUpdateMachineValidationExternalConfig").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	err := workflow.ExecuteActivity(ctx, manager.AddUpdateMachineValidationExternalConfigOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "AddUpdateMachineValidationExternalConfigOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}

// RemoveMachineValidationExternalConfig is a workflow to add machine validation test using RemoveMachineValidationExternalConfigOnSite activity
func RemoveMachineValidationExternalConfig(ctx workflow.Context, request *corev1.RemoveMachineValidationExternalConfigRequest) error {
	logger := log.With().Str("Workflow", "RemoveMachineValidationExternalConfig").Logger()

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

	// Invoke activity
	var manager activity.ManageMachineValidation

	err := workflow.ExecuteActivity(ctx, manager.RemoveMachineValidationExternalConfigOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "AddUpdateMachineValidationExternalConfigOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow")

	return nil
}
