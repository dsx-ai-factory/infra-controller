// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package inventory holds the settings every Cloud inventory workflow shares.
package inventory

import (
	"time"

	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/workflow"
)

const (
	// ActivityStartToCloseTimeout bounds one page of inventory, which the Site
	// Agent caps at 25 objects.
	//
	// The floor is the work itself. The heaviest pages are Instance Type,
	// Machine, and Instance, which reach roughly 3,100 sequential database round
	// trips for a full page because they loop over each object's capabilities or
	// interfaces. At 5ms per round trip that is about 15s, so this leaves roughly
	// four times the measured worst case.
	//
	// The ceiling is the retry budget. A page that exhausts ActivityMaximumAttempts
	// costs 2 x this + ActivityInitialInterval, and that total has to stay inside
	// cutil.DefaultInventoryReceiptInterval so a fully retried cycle does not run
	// into the next one. 60s puts the total at 125s against a 180s interval.
	//
	// Anything that waits on another service inside the activity is bound by this
	// too, so an inner deadline longer than this can never be reached. Site Agent
	// SSH Key Group sync is the one place that happens, and it sets its own
	// timeout rather than using this.
	ActivityStartToCloseTimeout = 60 * time.Second

	// ActivityMaximumAttempts retries a page once. A second failure means the page
	// is not landing, and retrying past that only delays the next cycle, which
	// carries the same data anyway.
	ActivityMaximumAttempts = 2

	// ActivityInitialInterval is the wait before the single retry.
	ActivityInitialInterval = 5 * time.Second

	// ActivityBackoffCoefficient grows the wait between attempts.
	ActivityBackoffCoefficient = 2.0

	// ActivityMaximumInterval caps the wait between attempts.
	ActivityMaximumInterval = 30 * time.Second
)

// ActivityOptions returns the Temporal activity options every inventory workflow
// uses. Shared rather than repeated per workflow, because the timeouts only mean
// something as a set: they are chosen against one page size and one retry budget,
// and a workflow that drifts from them silently opts out of both.
func ActivityOptions() workflow.ActivityOptions {
	return workflow.ActivityOptions{
		StartToCloseTimeout: ActivityStartToCloseTimeout,
		RetryPolicy: &temporal.RetryPolicy{
			InitialInterval:    ActivityInitialInterval,
			BackoffCoefficient: ActivityBackoffCoefficient,
			MaximumInterval:    ActivityMaximumInterval,
			MaximumAttempts:    ActivityMaximumAttempts,
		},
	}
}
