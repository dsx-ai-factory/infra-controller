// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package user

import (
	"context"
	"errors"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	cloudutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	userActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/user"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

const (
	// WorkflowIDPrefixUpdateUserFromNGC is the prefix for the UpdateUserFromNGC workflow ID
	WorkflowIDPrefixUpdateUserFromNGC = "user_update_from_ngc_"
	// WorkflowIDPrefixUpdateUserFromNGCImmediate is the prefix for the UpdateUserFromNGC immediate workflow ID
	WorkflowIDPrefixUpdateUserFromNGCImmediate = "user_update_from_ngc_immediate_"
)

// UpdateUserFromNGC is a Temporal workflow to fetch user data from NGC and update NICo DB
func UpdateUserFromNGC(ctx workflow.Context, userID uuid.UUID, encryptedNgcToken []byte, immediate bool) error {
	logger := log.With().Str("Workflow", "UpdateUserFromNGC").Str("User ID", userID.String()).Logger()

	logger.Info().Msg("starting activity")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    5 * time.Second,
		BackoffCoefficient: 1.5,
		MaximumInterval:    30 * time.Second,
		MaximumAttempts:    5,
	}

	if immediate {
		retrypolicy.MaximumAttempts = 1
	}

	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var userProfile userActivity.ManageUser

	var ngcUser userActivity.NgcUser
	err := workflow.ExecuteActivity(ctx, userProfile.GetUserDataFromNgc, userID, encryptedNgcToken).Get(ctx, &ngcUser)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: GetUserDataFromNgc")
		return err
	}

	err = workflow.ExecuteActivity(ctx, userProfile.UpdateUserInDB, userID, &ngcUser).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: UpdateUserInDB")
		return err
	}

	logger.Info().Msg("completed workflow")

	return nil
}

// UpdateUserFromNGCWithAuxiliaryID is a Temporal workflow to fetch user data from NGC and update NICo DB
func UpdateUserFromNGCWithAuxiliaryID(ctx workflow.Context, auxiliaryID string, encryptedNgcToken []byte, immediate bool) error {
	logger := log.With().Str("Workflow", "UpdateUserFromNGC").Str("User ClientID", auxiliaryID).Logger()

	logger.Info().Msg("starting activity")

	// RetryPolicy specifies how to automatically handle retries if an Activity fails.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    5 * time.Second,
		BackoffCoefficient: 1.5,
		MaximumInterval:    30 * time.Second,
		MaximumAttempts:    5,
	}

	if immediate {
		retrypolicy.MaximumAttempts = 1
	}

	options := workflow.ActivityOptions{
		// Timeout options specify when to automatically timeout Activity functions.
		StartToCloseTimeout: time.Minute,
		// Optionally provide a customized RetryPolicy.
		RetryPolicy: retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var userProfile userActivity.ManageUser

	var ngcUser userActivity.NgcUser
	err := workflow.ExecuteActivity(ctx, userProfile.GetUserDataFromNgcWithAuxiliaryID, auxiliaryID, encryptedNgcToken).Get(ctx, &ngcUser)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: GetUserDataFromNgcWithAuxiliaryID")
		return err
	}

	err = workflow.ExecuteActivity(ctx, userProfile.CreateOrUpdateUserInDBWithAuxiliaryID, &ngcUser).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: CreateOrUpdateUserInDBWithAuxiliaryID")
		return err
	}

	logger.Info().Msg("completed workflow")

	return nil
}

// ExecuteUpdateUserFromNGCWorkflow is a helper function to trigger execution of the UpdateUserFromNGC workflow
func ExecuteUpdateUserFromNGCWorkflow(ctx context.Context, tc client.Client, userID uuid.UUID, starfleetID string, ngcToken string, immediate bool) (*string, error) {
	// Allow only one user update workflow to execute at any given time per user ID

	// Use consistent ID for immediate execution so multiple parallel executions coalesce into one
	workflowID := WorkflowIDPrefixUpdateUserFromNGC + userID.String()
	if immediate {
		workflowID = WorkflowIDPrefixUpdateUserFromNGCImmediate + userID.String()
	}

	workflowOptions := client.StartWorkflowOptions{
		ID:                    workflowID,
		TaskQueue:             queue.CloudTaskQueue,
		WorkflowIDReusePolicy: temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
	}

	// Encrypt user token using Starfleet ID. Arguments provided to Temporal are stored in
	// Temporal DB so we don't want any plaintext keys/tokens
	//
	// Starfleet ID itself is _not_ passed as a workflow argument
	encryptedNgcToken := cloudutils.EncryptData([]byte(ngcToken), starfleetID)

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, UpdateUserFromNGC, userID, encryptedNgcToken, immediate)

	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: UpdateUserFromNGC")
		return nil, err
	}

	if immediate {
		// Execute the workflow synchronously
		err = we.Get(ctx, nil)
		if err != nil {
			log.Error().Err(err).Msg("failed to execute workflow: UpdateUserFromNGCImmediate")
			return nil, err
		}
	}

	wid := we.GetID()

	return &wid, nil
}

// ExecuteUpdateUserFromNGCWithAuxiliaryIDWorkflow is a helper function to trigger execution of the UpdateUserFromNGCWithAuxiliaryID workflow
func ExecuteUpdateUserFromNGCWithAuxiliaryIDWorkflow(ctx context.Context, tc client.Client, auxiliaryID string, ngcToken, encryptionKey string, immediate bool) (*string, error) {
	// Allow only one user update workflow to execute at any given time per user ID

	// Use consistent ID for immediate execution so multiple parallel executions coalesce into one
	workflowID := WorkflowIDPrefixUpdateUserFromNGC + auxiliaryID
	if immediate {
		workflowID = WorkflowIDPrefixUpdateUserFromNGCImmediate + auxiliaryID
	}

	workflowOptions := client.StartWorkflowOptions{
		ID:                    workflowID,
		TaskQueue:             queue.CloudTaskQueue,
		WorkflowIDReusePolicy: temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
	}

	if encryptionKey == "" {
		log.Error().Msg("encryption key is empty")
		return nil, errors.New("encryption key is empty")
	}

	encryptedNgcToken := cloudutils.EncryptData([]byte(ngcToken), encryptionKey)
	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, UpdateUserFromNGCWithAuxiliaryID, auxiliaryID, encryptedNgcToken, immediate)

	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: UpdateUserFromNGCWithAuxiliaryID")
		return nil, err
	}

	if immediate {
		// Execute the workflow synchronously
		err = we.Get(ctx, nil)
		if err != nil {
			log.Error().Err(err).Msg("failed to execute workflow: UpdateUserFromNGCWithAuxiliaryIDImmediate")
			return nil, err
		}
	}

	wid := we.GetID()

	return &wid, nil
}
