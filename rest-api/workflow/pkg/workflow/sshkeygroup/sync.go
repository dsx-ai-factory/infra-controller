// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"context"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	temporalEnums "go.temporal.io/api/enums/v1"

	"go.temporal.io/sdk/client"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// SyncSSHKeyGroup is a Temporal workflow to create/update SSH Key Group via Site Agent
func SyncSSHKeyGroup(ctx workflow.Context, siteID uuid.UUID, sshKeyGroupID uuid.UUID, version string) error {
	logger := log.With().Str("Workflow", "SyncSSHKeyGroup").Str("Site ID", siteID.String()).
		Str("SSHKeyGroupID ID", sshKeyGroupID.String()).Logger()

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

	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	// Sync SSH Key Group via Site Agent
	err := workflow.ExecuteActivity(ctx, sshKeyGroupManager.SyncSSHKeyGroupViaSiteAgent, siteID, sshKeyGroupID, version).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: SyncSSHKeyGroupViaSiteAgent")
		return err
	}

	// Update overall SSH Key Group status in DB
	serr := workflow.ExecuteActivity(ctx, sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB, sshKeyGroupID.String()).Get(ctx, nil)
	if serr != nil {
		// Log error but continue as we don't want to fail the entire workflow if sync was successful
		logger.Warn().Err(serr).Msg("failed to execute activity: UpdateSSHKeyGroupStatusInDB")
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteSyncSSHKeyGroupWorkflow is a helper function to trigger workflow to sync an SSHKeyGroup to a Site
func ExecuteSyncSSHKeyGroupWorkflow(ctx context.Context, tc client.Client, siteID uuid.UUID, sshkeyGroupID uuid.UUID, version string) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:                    "ssh-key-group-sync-" + siteID.String() + "-" + sshkeyGroupID.String() + "-" + version,
		TaskQueue:             queue.CloudTaskQueue,
		WorkflowIDReusePolicy: temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, SyncSSHKeyGroup, siteID, sshkeyGroupID, version)

	if err != nil {
		log.Error().Err(err).Msg("failed to execute SyncSSHKeyGroup workflow")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}
