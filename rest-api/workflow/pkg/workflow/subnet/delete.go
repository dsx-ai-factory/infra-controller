// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	subnetActivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
)

// DeleteSubnetByID is a helper Temporal workflow to delete an existing Subnet by ID
// This workflow is useful for invoking from Temporal CLI because it does not require us to create a proto request object
func DeleteSubnetByID(ctx workflow.Context, subnetID uuid.UUID) error {
	logger := log.With().Str("Workflow", "Subnet").Str("Action", "Delete").Str("Subnet ID", subnetID.String()).Logger()

	logger.Info().Msg("starting workflow")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    2 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    2 * time.Minute,
		MaximumAttempts:    10,
	}
	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: 3 * time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var subnetManager subnetActivity.ManageSubnet

	request := &corev1.NetworkSegmentDeletionRequest{
		Id: &corev1.NetworkSegmentId{Value: subnetID.String()},
	}

	err := workflow.ExecuteActivity(ctx, subnetManager.DeleteSubnetOnSite, request).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to delete activity: DeleteSubnetOnSite")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}
