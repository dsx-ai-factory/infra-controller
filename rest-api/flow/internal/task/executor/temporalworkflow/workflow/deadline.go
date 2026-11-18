// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"time"

	"go.temporal.io/sdk/workflow"
)

// executeActivityBeforeDeadline bounds queueing, retries and caller waiting by
// an operation-owned absolute deadline, preserving tighter activity limits.
// Cancellation stops Flow's wait; it cannot undo work accepted downstream.
func executeActivityBeforeDeadline(
	ctx workflow.Context,
	deadline time.Time,
	result any,
	activityType any,
	args ...any,
) error {
	if ctx.Err() != nil {
		return ctx.Err()
	}
	remaining := deadline.Sub(workflow.Now(ctx))
	if remaining <= 0 {
		return context.DeadlineExceeded
	}
	queryCtx, cancel := workflow.WithCancel(ctx)
	defer cancel()
	opts := workflow.GetActivityOptions(ctx)
	if opts.StartToCloseTimeout <= 0 || opts.StartToCloseTimeout > remaining {
		opts.StartToCloseTimeout = remaining
	}
	if opts.ScheduleToCloseTimeout <= 0 || opts.ScheduleToCloseTimeout > remaining {
		opts.ScheduleToCloseTimeout = remaining
	}
	queryCtx = workflow.WithActivityOptions(queryCtx, opts)
	future := workflow.ExecuteActivity(queryCtx, activityType, args...)
	timer := workflow.NewTimer(queryCtx, remaining)
	var err error
	selector := workflow.NewSelector(ctx)
	selector.AddFuture(future, func(f workflow.Future) { err = f.Get(ctx, result) })
	selector.AddFuture(timer, func(f workflow.Future) {
		err = f.Get(ctx, nil)
		if err == nil {
			err = context.DeadlineExceeded
		}
	})
	selector.Select(ctx)
	if ctx.Err() != nil {
		return ctx.Err()
	}
	// Workflow task dispatch can lag behind activity completion. Preserve the
	// selected branch instead of reclassifying its result using workflow.Now.
	return err
}

// sleepBeforeDeadline never grants a poll a fresh timeout window.
func sleepBeforeDeadline(ctx workflow.Context, deadline time.Time, interval time.Duration) error {
	return workflow.Sleep(ctx, min(interval, max(0, deadline.Sub(workflow.Now(ctx)))))
}
