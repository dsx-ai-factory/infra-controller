// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package domain

import (
	"context"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/domain"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

func ReconcileReservedDomains(ctx workflow.Context) error {
	ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: 10 * time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 1},
	})
	var manager domain.ManageDomain
	return workflow.ExecuteActivity(ctx, manager.ReconcileReservedDomains).Get(ctx, nil)
}

func ExecuteReconcileReservedDomains(ctx context.Context, tc client.Client) error {
	_, err := tc.ExecuteWorkflow(ctx, client.StartWorkflowOptions{
		ID: "domain-reserved-intent-recovery", CronSchedule: "@every 2m", TaskQueue: queue.CloudTaskQueue,
	}, ReconcileReservedDomains)
	return err
}
