// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
)

// dispatcher is the per-policy component of relay g2.
// It decides when and how events are sent from the relay queue to the worker.
type dispatcher interface {
	// run is the g2 event loop. It reads from r.notifyCh, dequeues events
	// from r, and sends workItems to workCh. It blocks until it determines
	// it should exit (notifyCh closed by g1 meaning trigger exhausted, or
	// forceCh closed by forceStop for immediate exit).
	run(r *relay, workCh chan<- workItem)
}

// newPolicyDispatcher returns the dispatcher implementation for the given policy.
func newPolicyDispatcher(p types.Policy) dispatcher {
	switch p {
	case types.Queue:
		return &queueDispatcher{}
	case types.QueueAll:
		return &queueAllDispatcher{}
	case types.Replace:
		return &replaceDispatcher{}
	default: // types.Skip
		return &skipDispatcher{}
	}
}

// --- skipDispatcher ---

// skipDispatcher drops the event when the worker is busy (non-blocking send).
type skipDispatcher struct{}

func (d *skipDispatcher) run(
	r *relay, workCh chan<- workItem,
) {
	for {
		select {
		case _, ok := <-r.notifyCh:
			if !ok {
				return // trigger exhausted
			}
		case <-r.forceCh:
			return // force stop: queue already cleared by relay.forceStop
		}

		r.mu.Lock()
		if len(r.queue) == 0 {
			r.mu.Unlock()
			continue
		}

		// Dispatch the oldest one and clear all the others in the queue.
		ev := r.queue[0]
		r.queue = r.queue[:0]
		r.mu.Unlock()

		jobCtx, jobCancel := context.WithCancel(r.forceCtx)
		select {
		case workCh <- workItem{ctx: jobCtx, cancel: jobCancel, ev: ev}:
		default:
			jobCancel() // worker busy; drop event
			log.Debug().Str("job", r.entry.job.Name()).
				Msg("skip: worker busy, dropping event")
		}
	}
}

// --- queueDispatcher ---

// queueDispatcher keeps only the latest pending event; blocks until the worker
// accepts it. Earlier events in the queue are discarded.
type queueDispatcher struct{}

func (d *queueDispatcher) run(
	r *relay, workCh chan<- workItem,
) {
	// The helper function drains the queue and delivers the latest event
	// to the worker after the trigger is exhausted (notifyCh closed).
	drainOnExhausted := func() {
		r.mu.Lock()
		if len(r.queue) == 0 {
			r.mu.Unlock()
			return
		}
		ev := r.queue[len(r.queue)-1]
		r.queue = r.queue[:0]
		r.mu.Unlock()

		jobCtx, jobCancel := context.WithCancel(r.forceCtx)
		select {
		case workCh <- workItem{ctx: jobCtx, cancel: jobCancel, ev: ev}:
		case <-r.forceCh:
			jobCancel()
			return
		}
	}

	for {
		// Wait for a notification that the queue has a new event.
		select {
		case _, ok := <-r.notifyCh:
			if !ok {
				// Trigger exhausted, drain the queue and exit.
				drainOnExhausted()
				return
			}
		case <-r.forceCh:
			return // force stop: queue already cleared by relay.forceStop
		}

		// There are events in the queue, process them.
		for {
			// Dequeue the latest event and discard earlier ones.
			r.mu.Lock()
			if len(r.queue) == 0 {
				r.mu.Unlock()
				// Queue is empty, break the loop and wait for notifications
				// on new events.
				break
			}

			ev := r.queue[len(r.queue)-1]
			r.queue = r.queue[:0]
			r.mu.Unlock()

			jobCtx, jobCancel := context.WithCancel(r.forceCtx)
			select {
			case workCh <- workItem{ctx: jobCtx, cancel: jobCancel, ev: ev}:
			case _, ok := <-r.notifyCh:
				// A newer event arrived; cancel the current pending job and
				// update ev to the latest in the queue (if any), then put it
				// back so the inner loop picks it up on the next iteration.
				// append reuses the cleared backing array — no allocation.
				jobCancel()

				r.mu.Lock()
				if len(r.queue) > 0 {
					ev = r.queue[len(r.queue)-1]
					r.queue = r.queue[:0]
				}
				r.queue = append(r.queue, ev)
				r.mu.Unlock()

				if !ok {
					// Trigger exhausted, drain the queue and exit.
					drainOnExhausted()
					return
				}
			case <-r.forceCh:
				jobCancel()
				return
			}
		}
	}
}

// --- queueAllDispatcher ---

// queueAllDispatcher delivers every event in FIFO order. On graceful shutdown
// it flushes any remaining queued events before exiting.
type queueAllDispatcher struct{}

func (d *queueAllDispatcher) run(
	r *relay, workCh chan<- workItem,
) {
	for {
		// Snapshot and clear the entire queue under a single lock,
		// then deliver events one-by-one without holding the mutex.
		// This minimises lock contention with relay g1.
		d.deliver(r, workCh)

		// Queue empty: wait for more events or shutdown.
		select {
		case _, ok := <-r.notifyCh:
			if !ok {
				// g1 has exited: no more events will be produced.
				// Flush anything added between our last drain and g1's exit.
				d.deliver(r, workCh)
				return
			}
			// More items enqueued; loop back to drain.
		case <-r.forceCh:
			return // force stop: skip drain, queue already cleared
		}
	}
}

// deliver snapshots and clears r.queue under a single lock, then sends each
// event to the worker one-by-one. Job contexts are derived from r.forceCtx,
// so force-stop cancels all in-flight jobs atomically via forceCancel.
func (d *queueAllDispatcher) deliver(r *relay, workCh chan<- workItem) {
	r.mu.Lock()
	batch := make([]types.Event, len(r.queue))
	copy(batch, r.queue)
	r.queue = r.queue[:0]
	r.mu.Unlock()

	for _, ev := range batch {
		jobCtx, jobCancel := context.WithCancel(r.forceCtx)
		select {
		case workCh <- workItem{ctx: jobCtx, cancel: jobCancel, ev: ev}:
		case <-r.forceCh:
			// Force stop fired while blocked on a send; discard remaining
			// batch and release the unsent context.
			jobCancel()
			return
		}
	}
}

// --- replaceDispatcher ---

// replaceDispatcher cancels the current job and starts a new one on each event.
type replaceDispatcher struct{}

func (d *replaceDispatcher) run(
	r *relay, workCh chan<- workItem,
) {
	// cancelPrev tracks the cancel function of the currently running job so
	// it can be cancelled when a newer event supersedes it. It is only
	// accessed from this goroutine, so no mutex is needed.
	var cancelPrev context.CancelFunc

	for {
		// Wait for a notification that the queue has a new event.
		// notifyCh has capacity 1 with non-blocking sends, so the queue
		// may hold unnotified events when it closes — exhausted drives the
		// drain-and-exit behaviour below.
		exhausted := false
		select {
		case _, ok := <-r.notifyCh:
			if !ok {
				exhausted = true
			}
		case <-r.forceCh:
			return // force stop: queue already cleared by relay.forceStop
		}

		r.mu.Lock()
		if len(r.queue) == 0 {
			r.mu.Unlock()
			if exhausted {
				return
			}
			continue
		}
		// Take the latest event and drop everything else: earlier events
		// are superseded and no longer relevant.
		ev := r.queue[len(r.queue)-1]
		r.queue = r.queue[:0]
		r.mu.Unlock()

		// Cancel the running job before dispatching the replacement.
		if cancelPrev != nil {
			cancelPrev()
		}

		jobCtx, jobCancel := context.WithCancel(r.forceCtx)
		select {
		case workCh <- workItem{ctx: jobCtx, cancel: jobCancel, ev: ev}:
			cancelPrev = jobCancel // track for cancel-on-replace
		case <-r.forceCh:
			jobCancel()
			return
		}

		if exhausted {
			return
		}
	}
}
