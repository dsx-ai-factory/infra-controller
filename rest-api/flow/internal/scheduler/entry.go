// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
)

// maxQueueSize is the default upper limit for the relay's in-memory queue.
// When the queue is full the oldest event is dropped to make room for the new
// one, preventing unbounded memory growth if the worker falls far behind.
const maxQueueSize = 2048

// entry pairs a Job with its Trigger, Policy, and the channels that wire the
// pipeline together. Entries are created internally by Scheduler.Schedule and
// hidden from the callers.
type entry struct {
	job     types.Job
	trigger types.Trigger
	policy  types.Policy
	eventCh chan types.Event // Trigger → relay  (capacity 1)
	workCh  chan workItem    // relay → worker   (unbuffered)
	relay   *relay           // created and assigned in Scheduler.Start
}

// workItem is the unit of work dispatched from relay to worker.
// ctx is a cancellable context for the job, derived from relay.forceCtx so
// that relay.forceStop cancels all in-flight jobs atomically by cancelling
// the parent context — no per-job registration race is possible.
type workItem struct {
	ctx    context.Context
	cancel context.CancelFunc
	ev     types.Event
}
