// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"sync"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
)

// relay bridges the Trigger channel to the Worker channel for a single entry.
//
// Two goroutines run inside relay.run:
//
//	g1 (intake):  reads events from entry.eventCh and appends them to the
//	              in-memory queue.
//	g2 (dispatch): delegates to the per-policy dispatcher, which dequeues
//	              events and sends workItems to entry.workCh.
//
// g1 signals g2 via notifyCh (capacity 1) each time it enqueues an event.
// g1 closes notifyCh on exit; dispatchers detect this (ok=false) to know
// the trigger is exhausted and no more events will ever arrive.
//
// forceCh is closed by forceStop to signal dispatchers to exit immediately
// without draining the queue.
type relay struct {
	entry       *entry
	mu          sync.Mutex
	queue       []types.Event
	notifyCh    chan struct{}      // g1 → g2 signal (cap 1); closed by g1 on exit
	forceCh     chan struct{}      // closed by forceStop; signals immediate exit
	forceCtx    context.Context    // parent context for all job contexts
	forceCancel context.CancelFunc // cancelled by forceStop to abort all in-flight jobs
	dispatcher  dispatcher
}

func newRelay(e *entry) *relay {
	forceCtx, forceCancel := context.WithCancel(context.Background())
	return &relay{
		entry:       e,
		notifyCh:    make(chan struct{}, 1),
		forceCh:     make(chan struct{}),
		forceCtx:    forceCtx,
		forceCancel: forceCancel,
		dispatcher:  newPolicyDispatcher(e.policy),
	}
}

// run starts g1 (as a goroutine) and runs g2 (dispatcher.run) inline.
// It closes entry.workCh on return to signal the worker.
func (r *relay) run() {
	defer close(r.entry.workCh)

	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		r.intake()
	}()

	r.dispatcher.run(r, r.entry.workCh) // blocks until g2 is done
	wg.Wait()                           // wait for g1 to exit
}

// intake (g1) reads events from entry.eventCh, appends them to the queue, and
// pings g2 via notifyCh. It exits when eventCh is closed (by the trigger
// goroutine), closing notifyCh to signal g2 that no more events will arrive.
// g1 never selects on ctx directly; the trigger goroutine is the sole consumer
// of runCtx and closes eventCh when it stops, which in turn stops g1.
func (r *relay) intake() {
	defer close(r.notifyCh)

	for ev := range r.entry.eventCh {
		r.mu.Lock()
		if len(r.queue) >= maxQueueSize {
			// Drop the oldest event to make room, preserving recent state.
			r.queue = r.queue[1:]
			log.Warn().Str("job", r.entry.job.Name()).
				Msg("relay queue full, dropping oldest event")
		}
		r.queue = append(r.queue, ev)
		r.mu.Unlock()
		// Non-blocking ping: if notifyCh already has a pending signal,
		// g2 will pick up the newly added item on its next wake.
		select {
		case r.notifyCh <- struct{}{}:
		default:
		}
	}
}

// forceStop clears the queue, cancels any in-flight worker job, and signals
// all dispatchers to exit immediately without draining.
// Called by Scheduler.Stop(true) before cancelling the run context.
func (r *relay) forceStop() {
	r.mu.Lock()
	r.queue = r.queue[:0]
	r.mu.Unlock()
	// Close forceCh before cancelling job contexts. This ensures that any
	// dispatcher blocked on a workCh send sees forceCh (worker is still busy
	// with the current job) and exits without dispatching further events.
	// Calling forceCancel first would free the worker prematurely, allowing
	// the pending send to complete before the dispatcher sees forceCh.
	close(r.forceCh)
	r.forceCancel() // cancels all job contexts derived from forceCtx
}
