// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"context"
	"time"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

// DeleteSSHKeyGroup is a Temporal workflow to delete an existing SSHKeyGroup on a Site via Site Agent
func DeleteSSHKeyGroup(ctx workflow.Context, siteID uuid.UUID, sshkeyGroupID uuid.UUID) error {
	logger := log.With().Str("Workflow", "SSHKeyGroup").Str("Action", "Delete").Str("Site ID", siteID.String()).
		Str("SSHkeyGroupID ID", sshkeyGroupID.String()).Logger()

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

	err := workflow.ExecuteActivity(ctx, sshKeyGroupManager.DeleteSSHKeyGroupViaSiteAgent, siteID, sshkeyGroupID).Get(ctx, nil)
	if err != nil {
		logger.Warn().Err(err).Msg("failed to execute activity: DeleteSSHKeyGroupViaSiteAgent")
		return err
	}

	logger.Info().Msg("completing workflow")

	return nil
}

// ExecuteDeleteSSHKeyGroupWorkflow is a helper function to trigger workflow to delete an SSHKeyGroup on a Site
func ExecuteDeleteSSHKeyGroupWorkflow(ctx context.Context, tc client.Client, siteID uuid.UUID, sshkeyGroupID uuid.UUID) (*string, error) {
	workflowOptions := client.StartWorkflowOptions{
		ID:                    "ssh-key-group-delete-" + siteID.String() + "-" + sshkeyGroupID.String(),
		TaskQueue:             queue.CloudTaskQueue,
		WorkflowIDReusePolicy: temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
	}

	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, DeleteSSHKeyGroup, siteID, sshkeyGroupID)

	if err != nil {
		log.Error().Err(err).Msg("failed to execute workflow: DeleteSSHKeyGroup")
		return nil, err
	}

	wid := we.GetID()

	return &wid, nil
}
