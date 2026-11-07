// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"time"

	"go.temporal.io/sdk/temporal"

	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/workflow"
)

const credentialRotationActivityTimeout = 2 * time.Minute

// RotateTemporalCertAccessOTP is a workflow to receive and process Base64 encoded encrypted OTP using ReceiveAndSaveOTP activity
func (bs *BoostrapAPI) RotateTemporalCertAccessOTP(ctx workflow.Context, base64EncodedEncryptedOTP string) error {
	logger := log.With().Str("Workflow", "RotateTemporalCertAccessOTP").Logger()
	logger.Info().Msg("Starting workflow to receive and process OTP")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    1 * time.Second,
		BackoffCoefficient: 2.0,
		MaximumInterval:    10 * time.Second,
		MaximumAttempts:    15,
	}

	options := workflow.ActivityOptions{
		StartToCloseTimeout: credentialRotationActivityTimeout,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	// Invoke ReceiveAndSaveOTP activity with Base64 encoded OTP
	err := workflow.ExecuteActivity(ctx, "ReceiveAndSaveOTP", base64EncodedEncryptedOTP).Get(ctx, nil)
	if err != nil {
		logger.Error().Err(err).Str("Activity", "ReceiveAndSaveOTP").Msg("Failed to execute activity from workflow")
		return err
	}

	logger.Info().Msg("Completing workflow to receive and process OTP")

	return nil
}
