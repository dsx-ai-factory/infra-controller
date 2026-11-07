// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"fmt"
	"sync"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
)

// Scheduler manages a set of scheduled jobs. Register jobs with Schedule,
// then call Start once to launch all goroutines.
//
// Expected call ordering: all Schedule calls must precede Start; Stop must be
// called after Start and at most once. The scheduler is a one-shot object:
// once stopped it cannot be restarted because the internal channels are closed
// during shutdown. Individual methods are protected by an internal mutex, but
// calling them out of order returns an error.
type Scheduler struct {
	mu      sync.Mutex
	entries []entry
	wg      sync.WaitGroup
	cancel  context.CancelFunc
	started bool
	stopped bool
}

// New creates a new Scheduler.
func New() *Scheduler {
	return &Scheduler{}
}

// Schedule registers a job with its trigger and overlap policy.
// If job is nil, Schedule is a no-op and returns nil.
// Returns an error if called after Start.
func (s *Scheduler) Schedule(
	job types.Job,
	trigger types.Trigger,
	policy types.Policy,
) error {
	if job == nil {
		// Nothing to schedule, just return.
		return nil
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.started {
		// Scheduler already started, don't allow new schedules.
		return fmt.Errorf(
			"scheduler: Schedule called after Start (job %q)",
			job.Name(),
		)
	}

	s.entries = append(
		s.entries,
		entry{
			job:     job,
			trigger: trigger,
			policy:  policy,
			eventCh: make(chan types.Event, 1),
			workCh:  make(chan workItem),
		},
	)

	return nil
}

// Start launches all scheduled jobs in the background and returns
// immediately. Returns an error if called more than once or after Stop.
func (s *Scheduler) Start(ctx context.Context) error {
	s.mu.Lock()

	if s.stopped {
		s.mu.Unlock()
		return fmt.Errorf("scheduler: Start called after Stop")
	}

	if s.started {
		s.mu.Unlock()
		return fmt.Errorf("scheduler: Start called more than once")
	}

	runCtx, runCancel := context.WithCancel(ctx)
	s.cancel = runCancel

	// Initialise all relays under the mutex so that every entry.relay is
	// non-nil by the time started=true is visible to concurrent callers.
	// Stop(true) dereferences entry.relay; if it were nil the call would panic.
	for i := range s.entries {
		s.entries[i].relay = newRelay(&s.entries[i])
	}

	s.started = true

	s.mu.Unlock()

	for i := range s.entries {
		e := &s.entries[i]

		log.Info().
			Str("job", e.job.Name()).
			Str("trigger", e.trigger.Description()).
			Msg("scheduling job")

		// Start the goroutines per entry, tracked by s.wg.

		// 1. trigger wrapper: runs Trigger.Emit then closes eventCh.
		// Emit blocks until the trigger is exhausted or ctx is cancelled.
		// Closing eventCh causes g1 (intake) to exit, which in turn closes
		// notifyCh and signals g2 (dispatch) to stop.
		s.wg.Add(1)
		go func(e *entry) {
			defer s.wg.Done()
			e.trigger.Emit(runCtx, e.eventCh)
			close(e.eventCh)
		}(e)

		// 2. relay wrapper: runs relay.run (g1 + g2 internally).
		// relay.run does not receive runCtx; it stops via channel cascade:
		// trigger closes eventCh → g1 closes notifyCh → g2 exits → workCh closed.
		s.wg.Add(1)
		go func(e *entry) {
			defer s.wg.Done()
			e.relay.run() // closes e.workCh on return
		}(e)

		// 3. worker wrapper: runs worker.run until workCh is closed
		s.wg.Add(1)
		go func(e *entry) {
			defer s.wg.Done()
			(&worker{e.job}).run(e.workCh) // exits when workCh is closed
		}(e)
	}

	return nil
}

// Stop shuts down the scheduler and waits for all goroutines to exit.
// If force is false, the run context is cancelled and each dispatcher exits
// according to its policy: QueueAll flushes remaining queued events before
// returning; Skip, Queue, and Replace exit immediately without draining.
// If force is true, each relay clears its queue, cancels the in-flight job,
// and signals all dispatchers to exit immediately via forceCh before the
// run context is cancelled.
// Returns an error if called before Start.
func (s *Scheduler) Stop(force bool) error {
	s.mu.Lock()

	if !s.started {
		s.mu.Unlock()
		return fmt.Errorf("scheduler: Stop called before Start")
	}

	if s.stopped {
		s.mu.Unlock()
		return fmt.Errorf("scheduler: Stop called more than once")
	}

	runCancel := s.cancel
	s.cancel = nil
	s.stopped = true

	s.mu.Unlock()

	if force {
		for i := range s.entries {
			s.entries[i].relay.forceStop()
		}
	}

	runCancel()
	s.wg.Wait()

	// Cancel each relay's forceCtx to release the monitoring goroutine
	// allocated by context.WithCancel. All worker goroutines have exited by
	// this point (wg.Wait returned), so no job context is in use and
	// cancellation is purely a resource cleanup. In the force path this is a
	// no-op because forceStop already called forceCancel; in the graceful
	// path this is the sole call site.
	for i := range s.entries {
		s.entries[i].relay.forceCancel()
	}

	return nil
}
