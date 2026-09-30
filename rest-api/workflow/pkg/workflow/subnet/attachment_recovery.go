// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"context"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/subnet"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

// ReconcileSubnetAttachmentIntents processes only persisted immutable intents.
// The DB claim limits each tick to one row, with cross-replica leases and
// delayed retries; this workflow does not discover or adopt arbitrary segments.
func ReconcileSubnetAttachmentIntents(ctx workflow.Context) error {
	ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 13 * time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 1},
	})
	var manager subnet.ManageSubnet
	return workflow.ExecuteActivity(ctx, manager.ReconcileAttachmentIntents).Get(ctx, nil)
}

func ExecuteReconcileSubnetAttachmentIntents(ctx context.Context, tc client.Client) error {
	_, err := tc.ExecuteWorkflow(ctx, client.StartWorkflowOptions{
		ID:           "subnet-attachment-intent-recovery",
		CronSchedule: "@every 2m",
		TaskQueue:    queue.CloudTaskQueue,
	}, ReconcileSubnetAttachmentIntents)
	return err
}
