// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"fmt"
	"time"

	cwi "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/inventory"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"

	sshKeyGroupActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/sshkeygroup"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// UpdateSSHKeyGroupInventory is a workflow called by Site Agent to update SSHKeyGroupinventory for a Site
func UpdateSSHKeyGroupInventory(ctx workflow.Context, siteID string, sshKeyGroupInventory *corev1.SSHKeyGroupInventory) (err error) {
	logger := log.With().Str("Workflow", "UpdateSSHKeyGroupInventory").Str("Site ID", siteID).Logger()

	startTime := workflow.Now(ctx)

	logger.Info().Msg("starting workflow")

	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		logger.Warn().Err(err).Msg(fmt.Sprintf("workflow triggered with invalid site ID: %s", siteID))
		return err
	}

	// Deliberately not cwi.ActivityOptions. UpdateSSHKeyGroupsInDB is the one
	// inventory activity that waits on another service inside its per-object
	// loop: it starts a Site workflow per keyset and blocks on it for up to
	// cwutil.WorkflowContextTimeout. Under the shared 60s budget that inner
	// deadline is unreachable, so the activity would be killed mid-wait and its
	// termination path, which cleans up the orphaned Site workflow, would never
	// run. This has to stay above that inner wait.
	//
	// It buys headroom rather than correctness. The wait is per object and the
	// page holds up to 25, so a Site slow to apply keysets can still exhaust
	// this. Making the sync asynchronous, as Tenant and VPC already do, is what
	// would actually bound it.
	retrypolicy := &temporal.RetryPolicy{
		InitialInterval:    cwi.ActivityInitialInterval,
		BackoffCoefficient: cwi.ActivityBackoffCoefficient,
		MaximumInterval:    cwi.ActivityMaximumInterval,
		MaximumAttempts:    cwi.ActivityMaximumAttempts,
	}
	options := workflow.ActivityOptions{
		StartToCloseTimeout: 5 * time.Minute,
		RetryPolicy:         retrypolicy,
	}

	ctx = workflow.WithActivityOptions(ctx, options)

	var sshKeyGroupManager sshKeyGroupActivity.ManageSSHKeyGroup

	var sshKeyGroupIDStrs []string

	err = workflow.ExecuteActivity(ctx, sshKeyGroupManager.UpdateSSHKeyGroupsInDB, parsedSiteID, sshKeyGroupInventory).Get(ctx, &sshKeyGroupIDStrs)
	if err != nil {
		logger.Warn().Err(err).Msg("failed execute activity: UpdateSSHKeyGroupsInDB")
	} else {
		for _, sshKeyGroupIDStr := range sshKeyGroupIDStrs {
			serr := workflow.ExecuteActivity(ctx, sshKeyGroupManager.UpdateSSHKeyGroupStatusInDB, sshKeyGroupIDStr).Get(ctx, nil)
			if serr != nil {
				// Log error but continue as we don't want to interrupt inventory processing
				logger.Warn().Err(serr).Msg("failed to execute activity: UpdateSSHKeyGroupStatusInDB")
			}
		}
	}

	// Record latency for this inventory call
	var inventoryMetricsManager cwm.ManageInventoryMetrics

	serr := workflow.ExecuteActivity(ctx, inventoryMetricsManager.RecordLatency, parsedSiteID, "UpdateSSHKeyGroupInventory", err != nil, workflow.Now(ctx).Sub(startTime)).Get(ctx, nil)
	if serr != nil {
		logger.Warn().Err(serr).Msg("failed to execute activity: RecordLatency")
	}

	logger.Info().Msg("completing workflow")

	// Return original error from inventory activity, if any
	return err
}
