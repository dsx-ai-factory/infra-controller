// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package dispatchguard fences displaced task activities before hardware dispatch.
package dispatchguard

import "context"

type contextKey struct{}

// WithCheck attaches a persisted task-ownership check to an activity context.
// Readiness gates and component managers recheck it after waits, so terminating
// a task also fences activities already running when preemption was committed.
func WithCheck(ctx context.Context, check func(context.Context) error) context.Context {
	return context.WithValue(ctx, contextKey{}, check)
}

// Check verifies the activity still owns dispatch permission. Contexts outside
// task execution have no persisted check, but still respect cancellation.
func Check(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if check, ok := ctx.Value(contextKey{}).(func(context.Context) error); ok {
		return check(ctx)
	}
	return nil
}
