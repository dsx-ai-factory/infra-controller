// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bootstrap

import (
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/workflow"
)

// RegisterSubscriber registers Bootstrap workflows and activities with Temporal
func (api *BoostrapAPI) RegisterSubscriber() error {
	// Initialize logger
	logger := ManagerAccess.Data.EB.Log

	// Only master pod should watch for the OTP rotation workflow
	if !ManagerAccess.Conf.EB.IsMasterPod {
		return nil
	}

	// Register workflows
	wflowRegisterOptions := workflow.RegisterOptions{
		Name: "RotateTemporalCertAccessOTP",
	}
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflowWithOptions(api.RotateTemporalCertAccessOTP, wflowRegisterOptions)
	logger.Info().Msg("Bootstrap: Successfully registered RotateTemporalCertAccessOTP workflow")

	// Register activities
	otpHandler := NewOTPHandler(ManagerAccess.Data.EB.Managers.Bootstrap.Secret)

	activityRegisterOptions := activity.RegisterOptions{
		Name: "ReceiveAndSaveOTP",
	}
	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivityWithOptions(otpHandler.ReceiveAndSaveOTP, activityRegisterOptions)
	logger.Info().Msg("Bootstrap: Successfully registered ReceiveAndSaveOTP activity")

	return nil
}
