// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"errors"
	"os"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
)

func TestMain(m *testing.M) {
	zerolog.SetGlobalLevel(zerolog.Disabled)
	os.Exit(m.Run())
}

// --- helpers ---

type funcJob struct {
	name string
	run  func(ctx context.Context, ev types.Event) error
}

func (j *funcJob) Name() string { return j.name }
func (j *funcJob) Run(ctx context.Context, ev types.Event) error {
	return j.run(ctx, ev)
}

// exhaustNotifyTrigger wraps a Trigger and closes done when Emit returns.
// Used in tests to detect when a trigger has finished forwarding all events
// into entry.eventCh. This matters because the trigger goroutine is the only
// one that receives runCtx: if Stop cancels runCtx while Emit is still
// mid-forward, Emit exits early and any events not yet written into eventCh
// are lost. Waiting on done before calling Stop prevents that race.
type exhaustNotifyTrigger struct {
	inner types.Trigger
	done  chan struct{}
}

func (t *exhaustNotifyTrigger) Description() string { return t.inner.Description() }
func (t *exhaustNotifyTrigger) Emit(ctx context.Context, ch chan<- types.Event) {
	t.inner.Emit(ctx, ch)
	close(t.done)
}

// --- TestIntervalTrigger_Fires ---

func TestIntervalTrigger_Fires(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	trigger, err := types.NewIntervalTrigger(20 * time.Millisecond)
	require.NoError(t, err)
	ch := make(chan types.Event, 1)
	go func() {
		trigger.Emit(ctx, ch)
		close(ch)
	}()

	// Cancel after receiving 2 events. count may exceed 2 if the timer fires
	// again before Emit observes the cancellation, so assert >= 2.
	// The outer test timeout (-timeout flag) is the guard against hangs.
	var count int
	for range ch {
		count++
		if count == 2 {
			cancel()
		}
	}
	assert.GreaterOrEqual(t, count, 2, "expected at least 2 signals before cancel")
}

// --- TestIntervalTrigger_NonPositiveDuration ---

func TestIntervalTrigger_NonPositiveDuration(t *testing.T) {
	_, err := types.NewIntervalTrigger(0)
	assert.Error(t, err, "expected error for zero duration")

	_, err = types.NewIntervalTrigger(-time.Second)
	assert.Error(t, err, "expected error for negative duration")
}

// --- TestOnceTrigger_FiresOnce ---

func TestOnceTrigger_FiresOnce(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	trigger := types.NewOnceTrigger()
	ch := make(chan types.Event, 1)
	go func() {
		trigger.Emit(ctx, ch)
		close(ch)
	}()

	// Emit sends one event and returns; the wrapper closes ch naturally.
	// Drain the channel without any external cancellation so the test
	// genuinely verifies that OnceTrigger does not send a second event.
	var count int
	for range ch {
		count++
	}
	assert.Equal(t, 1, count, "OnceTrigger must fire exactly once")
}

// --- TestEventTrigger_FiresOnPublish ---

func TestEventTrigger_FiresOnPublish(t *testing.T) {
	src := make(chan types.Event, 1)
	trigger := types.NewEventTrigger(src)
	ch := make(chan types.Event, 1)
	go func() {
		trigger.Emit(context.Background(), ch)
		close(ch)
	}()

	src <- types.Event{
		Type:    types.EventLeakDetected,
		Payload: "machine-1",
	}
	close(src) // exhaust the trigger so Emit returns and closes ch

	e, ok := <-ch
	require.True(t, ok, "expected one event on ch")
	assert.Equal(t, types.EventLeakDetected, e.Type)
	assert.Equal(t, "machine-1", e.Payload)
}

// --- TestScheduler_SkipPolicy ---

// jobBlocker is a deterministic blocking primitive used by TestScheduler_SkipPolicy.
//
// The job calls wait(): it acquires mu, sets blocking=true, then calls
// cond.Wait() which atomically releases mu and suspends the goroutine.
// Because the bool is set while holding mu, and cond.Wait atomically releases
// mu before sleeping, there is no window between "blocking is visible" and
// "the goroutine is suspended". The test therefore calls release(), which
// spins until it observes blocking==true under mu, then signals the condvar —
// guaranteeing the signal always arrives after the goroutine is waiting.
type jobBlocker struct {
	mu       sync.Mutex
	cond     *sync.Cond
	blocking bool
}

func newJobBlocker() *jobBlocker {
	b := &jobBlocker{}
	b.cond = sync.NewCond(&b.mu)
	return b
}

func (b *jobBlocker) wait() {
	b.mu.Lock()
	b.blocking = true
	b.cond.Wait() // atomically releases mu and suspends
	b.mu.Unlock()
}

func (b *jobBlocker) release() {
	for {
		b.mu.Lock()
		if b.blocking {
			b.cond.Signal()
			b.mu.Unlock()
			return
		}
		b.mu.Unlock()
		time.Sleep(time.Millisecond)
	}
}

func TestScheduler_SkipPolicy(t *testing.T) {
	// skipDispatcher uses a non-blocking send to workCh: if the worker
	// goroutine hasn't been scheduled when the first event arrives, the event
	// is dropped. Pre-fill src with enough events so the dispatcher has
	// multiple retry opportunities before the worker goroutine is ready.
	const preFill = 50

	blocker := newJobBlocker()
	jobStarted := make(chan struct{})
	var startOnce sync.Once
	var callCount atomic.Int64

	src := make(chan types.Event, preFill)
	for range preFill {
		src <- types.Event{}
	}

	job := &funcJob{
		name: "skip-job",
		run: func(_ context.Context, ev types.Event) error {
			callCount.Add(1)
			// Only the first invocation blocks. Subsequent events that slip
			// through after the worker returns from cond.Wait are handled
			// gracefully: startOnce.Do is a no-op so they return immediately,
			// preventing a deadlock if release() has already been called.
			startOnce.Do(func() {
				close(jobStarted)
				blocker.wait()
			})
			return nil
		},
	}

	triggerExhausted := make(chan struct{})
	trig := &exhaustNotifyTrigger{
		inner: types.NewEventTrigger(src),
		done:  triggerExhausted,
	}

	s := New()
	require.NoError(t, s.Schedule(job, trig, types.Skip))
	require.NoError(t, s.Start(context.Background()))

	select {
	case <-jobStarted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for job to start")
	}

	// Close src so the trigger exhausts. Remaining buffered events are
	// forwarded into the relay pipeline and dropped by Skip (worker is busy).
	close(src)

	// Wait for the trigger to exhaust: all events have been written into
	// entry.eventCh and are now either in the relay queue or already
	// dispatched (and dropped) by Skip. Only then release the job, so the
	// worker cannot pick up any of those events after unblocking.
	select {
	case <-triggerExhausted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for trigger to exhaust")
	}

	// blocker.release() spins under the mutex until blocking==true, then
	// signals the condvar. Because blocker.wait() sets blocking=true while
	// holding the mutex before calling cond.Wait(), the signal is guaranteed
	// to arrive after the goroutine is suspended — no spurious wake-up risk.
	blocker.release()

	require.NoError(t, s.Stop(false))

	count := callCount.Load()
	assert.Positive(t, count, "expected at least one call")
	assert.Less(t, count, int64(preFill), "Skip should have dropped most events")
}

// --- TestScheduler_QueuePolicy ---

func TestScheduler_QueuePolicy(t *testing.T) {
	const extraEvents = 10

	jobStarted := make(chan struct{})
	release := make(chan struct{})
	src := make(chan types.Event, extraEvents+1)
	var processed []int

	job := &funcJob{
		name: "queued-job",
		run: func(_ context.Context, ev types.Event) error {
			if ev.Payload.(int) == 0 {
				close(jobStarted)
				<-release
			}
			processed = append(processed, ev.Payload.(int))
			return nil
		},
	}

	triggerExhausted := make(chan struct{})
	trig := &exhaustNotifyTrigger{
		inner: types.NewEventTrigger(src),
		done:  triggerExhausted,
	}

	s := New()
	require.NoError(t, s.Schedule(job, trig, types.Queue))
	require.NoError(t, s.Start(context.Background()))

	// Send event 0 to start the job. The worker should process
	// it and block on "release" channel.
	src <- types.Event{Payload: 0}

	// Wait for job 0 to be running (worker is busy).
	select {
	case <-jobStarted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for job to start")
	}

	// While the worker is blocked, send remaining events.
	for i := range extraEvents {
		src <- types.Event{Payload: i + 1}
	}
	close(src)

	// Wait for trigger exhaustion: EventTrigger.Emit has forwarded all events
	// into entry.eventCh, so they are in the relay queue awaiting dispatch
	// once job 0 finishes.
	select {
	case <-triggerExhausted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for trigger to exhaust")
	}

	close(release) // unblock job 0

	require.NoError(t, s.Stop(false))

	require.GreaterOrEqual(t, len(processed), 2)
	assert.Equal(t, 0, processed[0])
	assert.Equal(t, extraEvents, processed[len(processed)-1])
}

// --- TestScheduler_ReplacePolicy ---

func TestScheduler_ReplacePolicy(t *testing.T) {
	var firstCtxCancelled atomic.Bool
	firstStarted := make(chan struct{})
	secondDone := make(chan struct{})

	manualCh := make(chan types.Event, 2)

	job := &funcJob{
		name: "replace-job",
		run: func(jobCtx context.Context, ev types.Event) error {
			if ev.Payload.(int) == 1 {
				// Signal that the first run has started, then block until
				// Replace cancels its context.
				close(firstStarted)
				<-jobCtx.Done()
				firstCtxCancelled.Store(true)
			} else {
				close(secondDone)
			}
			return nil
		},
	}

	s := New()
	require.NoError(t, s.Schedule(job, types.NewEventTrigger(manualCh), types.Replace))
	require.NoError(t, s.Start(context.Background()))

	// Send the first event and wait for the job to start before sending the
	// second, so Replace definitely sees a running job to cancel.
	manualCh <- types.Event{Payload: 1}
	<-firstStarted
	manualCh <- types.Event{Payload: 2}
	<-secondDone
	require.NoError(t, s.Stop(false))

	assert.True(t, firstCtxCancelled.Load(), "expected first run context to be cancelled by Replace policy")
}

// --- TestScheduler_QueueAllPolicy ---

func TestScheduler_QueueAllPolicy(t *testing.T) {
	const numEvents = 5

	manualCh := make(chan types.Event, numEvents)

	// Job 0 records its event then blocks, causing events 1–4 to pile up in
	// the queue. All other jobs run without blocking.
	firstProcessed := make(chan struct{})
	release := make(chan struct{})
	var processed []int

	job := &funcJob{
		name: "queueall-job",
		run: func(_ context.Context, ev types.Event) error {
			processed = append(processed, ev.Payload.(int))
			if ev.Payload.(int) == 0 {
				close(firstProcessed)
				<-release
			}
			return nil
		},
	}

	// Wrap the trigger so we know when Emit has returned (all events
	// forwarded into entry.eventCh). EventTrigger forwards events one at a
	// time; it only returns after the last event (event 4) has been placed
	// into entry.eventCh, which guarantees events 0–3 are already in g1's
	// relay queue. Without this barrier, Stop(false) can cancel ctx while
	// EventTrigger is mid-forward, causing it to drop events that never reach
	// the relay queue.
	triggerExhausted := make(chan struct{})
	trig := &exhaustNotifyTrigger{
		inner: types.NewEventTrigger(manualCh),
		done:  triggerExhausted,
	}

	s := New()
	require.NoError(t, s.Schedule(job, trig, types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	// Send all events then exhaust the trigger.
	for i := range numEvents {
		manualCh <- types.Event{Payload: i}
	}
	close(manualCh)

	// Wait for job 0 to start blocking. This proves the pipeline is live:
	// the scheduler has received and begun processing events.
	select {
	case <-firstProcessed:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for first job to block")
	}

	// Wait for the trigger to exhaust: EventTrigger.Emit only returns after
	// successfully forwarding event 4 into entry.eventCh, which means events
	// 0–3 are already in relay.queue. This guarantees all 5 events are in the
	// pipeline before Stop is called. Without this barrier, Stop cancelling
	// runCtx while Emit is mid-forward would cause Emit to return early,
	// closing eventCh before all events have been written into it.
	select {
	case <-triggerExhausted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for trigger to exhaust")
	}

	// Unblock job 0 so the worker can accept the queued events, then stop
	// the scheduler. Stop waits for all goroutines to exit, so processed is
	// fully populated by the time it returns.
	close(release)
	require.NoError(t, s.Stop(false))

	// Stop's wg.Wait() only returns after the last job completes, so
	// processed is fully populated and safe to read without a mutex.
	assert.Equal(t, []int{0, 1, 2, 3, 4}, processed)
}

// --- TestScheduler_Stop_Graceful ---

func TestScheduler_Stop_Graceful(t *testing.T) {
	// Verify Stop(false) blocks until the currently-running job finishes.
	jobStarted := make(chan struct{})
	release := make(chan struct{})
	jobDone := make(chan struct{})

	job := &funcJob{
		name: "graceful-job",
		run: func(_ context.Context, _ types.Event) error {
			close(jobStarted)
			<-release
			close(jobDone) // signals just before the job returns
			return nil
		},
	}

	s := New()
	// QueueAll is required here. skipDispatcher sends to workCh with a
	// default case (non-blocking): if the worker goroutine has not been
	// scheduled yet when the OnceTrigger fires, the only event is dropped
	// and jobStarted is never closed. queueAllDispatcher.deliver blocks on
	// workCh (no default case) so the event is always delivered regardless
	// of goroutine scheduling order.
	require.NoError(t, s.Schedule(job, types.NewOnceTrigger(), types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	select {
	case <-jobStarted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for job to start")
	}

	// Unblock the job in a goroutine while calling Stop(false) inline.
	// Stop must not return until the job finishes; the job only finishes
	// after close(release) runs.
	go close(release)
	require.NoError(t, s.Stop(false))

	// Causal proof: Stop's wg.Wait() returns only after all goroutines exit.
	// The worker goroutine exits only after the job returns, which closes
	// jobDone. So if Stop returned, jobDone must already be closed.
	select {
	case <-jobDone:
	default:
		t.Error("Stop(false) returned before job completed")
	}
}

// --- TestScheduler_Stop_Force ---

func TestScheduler_Stop_Force(t *testing.T) {
	// The job blocks until its context is cancelled. Force stop must
	// cancel the running job and return promptly.
	started := make(chan struct{})
	jobCtxCancelled := make(chan struct{})

	job := &funcJob{
		name: "force-job",
		run: func(ctx context.Context, _ types.Event) error {
			close(started)
			<-ctx.Done()
			close(jobCtxCancelled)
			return nil
		},
	}

	s := New()
	// QueueAll guarantees delivery regardless of worker scheduling order;
	// Skip's non-blocking send can drop the event if the worker goroutine
	// hasn't been scheduled yet when OnceTrigger fires.
	require.NoError(t, s.Schedule(job, types.NewOnceTrigger(), types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	// Wait for job to start before force-stopping.
	select {
	case <-started:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for job to start")
	}

	require.NoError(t, s.Stop(true))

	select {
	case <-jobCtxCancelled:
	case <-t.Context().Done():
		t.Fatal("expected job context to be cancelled by force stop")
	}
}

// --- TestScheduler_TriggerExhaustion ---

func TestScheduler_TriggerExhaustion(t *testing.T) {
	// OnceTrigger exhausts after one event. The scheduler should exit
	// the pipeline cleanly without needing ctx cancellation.
	var callCount atomic.Int64
	done := make(chan struct{})

	job := &funcJob{
		name: "once-job",
		run: func(_ context.Context, _ types.Event) error {
			callCount.Add(1)
			close(done)
			return nil
		},
	}

	s := New()
	// QueueAll guarantees delivery; Skip's non-blocking send can drop the
	// event if the worker goroutine hasn't been scheduled yet.
	require.NoError(t, s.Schedule(job, types.NewOnceTrigger(), types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("job did not run within 1s")
	}

	require.NoError(t, s.Stop(false))
	assert.Equal(t, int64(1), callCount.Load(), "expected exactly 1 call")
}

// --- TestScheduler_ForceStop_QueueAll ---

func TestScheduler_ForceStop_QueueAll(t *testing.T) {
	// Force stop while QueueAll is waiting for events: the dispatcher
	// must exit via forceCh immediately without draining.
	jobStarted := make(chan struct{})
	release := make(chan struct{})
	var callCount atomic.Int64

	src := make(chan types.Event, 10)

	job := &funcJob{
		name: "force-queueall",
		run: func(ctx context.Context, _ types.Event) error {
			callCount.Add(1)
			select {
			case jobStarted <- struct{}{}:
			default:
			}
			select {
			case <-release:
			case <-ctx.Done():
			}
			return nil
		},
	}

	s := New()
	require.NoError(t, s.Schedule(job, types.NewEventTrigger(src), types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	// Trigger first job and queue more events behind it.
	src <- types.Event{}
	<-jobStarted
	for range 5 {
		src <- types.Event{}
	}

	stopReturned := make(chan struct{})
	go func() {
		require.NoError(t, s.Stop(true))
		close(stopReturned)
	}()

	select {
	case <-stopReturned:
	case <-time.After(time.Second):
		t.Error("Stop(true) did not return within 1s")
	}

	// Only the one running job should have been executed; queued events
	// must be dropped by force stop.
	assert.Equal(t, int64(1), callCount.Load(), "expected 1 job run after force stop")
}

// --- TestScheduler_QueueOverflow ---

func TestScheduler_QueueOverflow(t *testing.T) {
	// Flood the queue beyond maxQueueSize while the worker is blocked on
	// event 0. The scheduler must not deadlock, and the newest event must
	// always be retained after draining.
	//
	// The deterministic oldest-dropped invariant is verified separately in
	// TestRelay_IntakeQueueOverflow, which tests relay.intake directly without
	// dispatcher-goroutine races.
	const overflow = 10
	total := overflow + maxQueueSize // last payload; newest event

	firstStarted := make(chan struct{})
	release := make(chan struct{})
	var mu sync.Mutex
	var processed []int

	job := &funcJob{
		name: "overflow-job",
		run: func(_ context.Context, ev types.Event) error {
			id := ev.Payload.(int)
			if id == 0 {
				close(firstStarted)
				<-release
			}
			mu.Lock()
			processed = append(processed, id)
			mu.Unlock()
			return nil
		},
	}

	// Pre-fill src so the trigger drains independently of the test goroutine.
	src := make(chan types.Event, total+1)
	for i := range total + 1 {
		src <- types.Event{Payload: i}
	}
	close(src)

	triggerExhausted := make(chan struct{})
	trig := &exhaustNotifyTrigger{
		inner: types.NewEventTrigger(src),
		done:  triggerExhausted,
	}

	s := New()
	require.NoError(t, s.Schedule(job, trig, types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	// Wait for the first job to be running so we know the worker is blocked.
	select {
	case <-firstStarted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for first job to start")
	}

	// Wait for the trigger to exhaust: once Emit returns, all events have
	// been forwarded into entry.eventCh and are either in the relay queue or
	// in the dispatcher's in-flight pre-batch. The overflow-induced drops have
	// already occurred by this point.
	select {
	case <-triggerExhausted:
	case <-t.Context().Done():
		t.Fatal("timed out waiting for trigger to exhaust")
	}

	// Unblock the first job; the worker will now drain the queue.
	close(release)
	require.NoError(t, s.Stop(false))

	mu.Lock()
	defer mu.Unlock()

	// The newest event must always be retained regardless of how many were dropped.
	require.NotEmpty(t, processed)
	assert.Equal(t, total, processed[len(processed)-1],
		"newest event must always be retained")
}

// --- TestRelay_IntakeQueueOverflow ---

func TestRelay_IntakeQueueOverflow(t *testing.T) {
	// Directly test that relay.intake drops the OLDEST event when the queue
	// reaches maxQueueSize, keeping the most-recent events.
	// This test bypasses the dispatcher to eliminate goroutine-scheduling races.
	e := &entry{
		job:     &funcJob{name: "test", run: func(_ context.Context, _ types.Event) error { return nil }},
		eventCh: make(chan types.Event, 1),
		workCh:  make(chan workItem),
		// policy: leave it unspecified so the relay uses the default policy (Skip currently)
	}
	r := newRelay(e)
	defer r.forceCancel()

	const extra = 10
	total := maxQueueSize + extra

	go func() {
		for i := range total {
			e.eventCh <- types.Event{Payload: i}
		}
		close(e.eventCh)
	}()

	r.intake() // blocks until eventCh is closed

	require.Equal(t, maxQueueSize, len(r.queue), "queue must be capped at maxQueueSize")
	assert.Equal(t, extra, r.queue[0].Payload.(int),
		"oldest 'extra' events (0..extra-1) must be dropped; oldest retained is 'extra'")
	assert.Equal(t, total-1, r.queue[maxQueueSize-1].Payload.(int),
		"newest event must always be retained")
}

// --- TestScheduler_Schedule_AfterStart_Error ---

func TestScheduler_Schedule_AfterStart_Error(t *testing.T) {
	s := New()
	require.NoError(t, s.Start(context.Background()))

	job := &funcJob{
		name: "late-job",
		run:  func(_ context.Context, _ types.Event) error { return nil },
	}

	err := s.Schedule(job, types.NewOnceTrigger(), types.Skip)
	assert.Error(t, err, "expected error when Schedule called after Start")

	require.NoError(t, s.Stop(true))
}

// --- TestScheduler_Start_Twice_Error ---

func TestScheduler_Start_Twice_Error(t *testing.T) {
	s := New()
	require.NoError(t, s.Start(context.Background()))
	assert.Error(t, s.Start(context.Background()), "expected error on second Start call")
	require.NoError(t, s.Stop(true))
}

// --- TestScheduler_Start_AfterStop_Error ---

func TestScheduler_Start_AfterStop_Error(t *testing.T) {
	// The scheduler is a one-shot object. Restarting after Stop must be
	// rejected because internal channels are closed during shutdown.
	s := New()
	require.NoError(t, s.Start(context.Background()))
	require.NoError(t, s.Stop(true))
	assert.Error(t, s.Start(context.Background()), "expected error when Start called after Stop")
}

// --- TestScheduler_Stop_Twice_Error ---

func TestScheduler_Stop_Twice_Error(t *testing.T) {
	s := New()
	require.NoError(t, s.Start(context.Background()))
	require.NoError(t, s.Stop(true))
	assert.Error(t, s.Stop(true), "expected error on second Stop call")
}

// --- TestScheduler_JobError_DoesNotStop ---

func TestScheduler_JobError_DoesNotStop(t *testing.T) {
	const wantCalls = 3

	// Use a buffered source channel so we control exactly how many events
	// are generated, eliminating timing-dependent behaviour.
	src := make(chan types.Event, wantCalls)
	for range wantCalls {
		src <- types.Event{}
	}
	close(src) // exhausts the trigger after all events are consumed

	called := make(chan struct{}, wantCalls)

	job := &funcJob{
		name: "error-job",
		run: func(_ context.Context, _ types.Event) error {
			called <- struct{}{}
			return errors.New("always fails")
		},
	}

	s := New()
	// QueueAll ensures every event is delivered even while a prior run's
	// error is being handled, proving that job errors don't stall the
	// scheduler.
	require.NoError(t, s.Schedule(job, types.NewEventTrigger(src), types.QueueAll))
	require.NoError(t, s.Start(context.Background()))

	// Scheduler must process all wantCalls events despite repeated errors.
	for range wantCalls {
		select {
		case <-called:
		case <-time.After(5 * time.Second):
			t.Fatal("timed out waiting for call")
		}
	}

	require.NoError(t, s.Stop(false))
}
