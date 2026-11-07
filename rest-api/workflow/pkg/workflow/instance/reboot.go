// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	instanceActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// RebootInstanceByID is a helper Temporal workflow to reboot a Machine associated with an Instance
// This workflow is useful for invoking from Temporal CLI because it does not require us to create a proto request object
func RebootInstanceByID(ctx workflow.Context, instanceID uuid.UUID, rebootWithCustomIpxe bool, applyUpdatesOnReboot bool) error {
	logger := log.With().Str("Workflow", "Instance").Str("Action", "Reboot").Str("Instance ID", instanceID.String()).Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    2 * time.Minute,
		MaximumAttempts:    15,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 2 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var instanceManager instanceActivity.ManageInstance

	request := &corev1.InstancePowerRequest{
		InstanceId: &corev1.InstanceId{
			Value: instanceID.String(),
		},
		BootWithCustomIpxe:   rebootWithCustomIpxe,
		ApplyUpdatesOnReboot: applyUpdatesOnReboot,
	}

	err := workflow.ExecuteActivity(ctx, instanceManager.RebootInstanceOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "RebootInstanceOnSite").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
