// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"context"
	"fmt"
	"time"

	"github.com/robfig/cron/v3"
)

// Trigger defines when events are emitted to the scheduler.
// Implementations are called in a dedicated goroutine by the scheduler.
type Trigger interface {
	// Description returns a human-readable name for logging.
	Description() string

	// Emit writes events into ch until ctx is cancelled or the trigger
	// is exhausted. The scheduler closes ch after Emit returns.
	Emit(ctx context.Context, ch chan<- Event)
}

// --- IntervalTrigger ---

// IntervalTrigger fires on a fixed time interval.
type IntervalTrigger struct {
	interval time.Duration // how often to fire
}

// NewIntervalTrigger creates a Trigger that fires on a fixed time interval.
// Returns an error if interval is non-positive; time.NewTicker would panic
// with the same input.
func NewIntervalTrigger(interval time.Duration) (*IntervalTrigger, error) {
	if interval <= 0 {
		return nil, fmt.Errorf("interval must be positive, got %s", interval)
	}

	return &IntervalTrigger{interval: interval}, nil
}

// Description returns "interval(<duration>)", e.g. "interval(30s)".
func (t *IntervalTrigger) Description() string {
	return fmt.Sprintf("interval(%s)", t.interval)
}

// Emit sends an empty Event on ch after each interval tick until ctx is
// cancelled. The send into ch is itself guarded by ctx so that a
// cancellation arriving while the relay has not yet consumed the previous
// event does not leak this goroutine.
func (t *IntervalTrigger) Emit(ctx context.Context, ch chan<- Event) {
	ticker := time.NewTicker(t.interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			// ch has capacity 1. If the relay has not yet consumed the
			// previous event, this send blocks. The ctx.Done guard below
			// prevents a goroutine leak: without it, a context cancellation
			// arriving while we are blocked here would go unnoticed because
			// we already left the outer select and can never reach its
			// ctx.Done case.
			select {
			case ch <- Event{}:
			case <-ctx.Done():
				return
			}
		}
	}
}

// --- CronTrigger ---

// CronTrigger fires on a robfig/cron v3 6-field cron schedule
// (seconds minute hour dom month dow), e.g. "0 30 9 * * 1-5" for
// weekdays at 09:30:00.
//
// robfig/cron v3 defaults to 5-field (no seconds); WithSeconds() is used
// to enable the 6-field format with seconds as the first field.
//
// NOTE: This trigger is used by the internal system job scheduler.
// User-defined task schedules use 5-field cron expressions without seconds,
// parsed via cron.ParseStandard() in the task schedule dispatcher.
type CronTrigger struct {
	expr string // validated robfig/cron v3 6-field expression
}

// NewCronTrigger creates a Trigger that fires on a cron schedule.
// expr must be a robfig/cron v3 6-field expression, e.g. "0 0 2 * * *"
// for daily at 02:00:00. Returns an error if the expression is invalid.
func NewCronTrigger(expr string) (*CronTrigger, error) {
	// Validate the expression at construction time using the same parser
	// that Emit will use, so callers get an immediate error rather than a
	// silent no-op trigger at runtime.
	c := cron.New(cron.WithSeconds())
	if _, err := c.AddFunc(expr, func() {}); err != nil {
		return nil, fmt.Errorf("invalid cron expression %q: %w", expr, err)
	}

	return &CronTrigger{expr: expr}, nil
}

// Description returns "cron(<expr>)", e.g. "cron(0 0 2 * * *)".
func (t *CronTrigger) Description() string {
	return fmt.Sprintf("cron(%s)", t.expr)
}

// Emit starts the cron scheduler and sends an empty Event on ch at each
// scheduled time until ctx is cancelled. If the relay has not consumed the
// previous event when the next tick fires, that tick is dropped (the send
// into ch is non-blocking with a ctx.Done guard).
func (t *CronTrigger) Emit(ctx context.Context, ch chan<- Event) {
	c := cron.New(cron.WithSeconds())
	emitter := func() {
		select {
		case ch <- Event{}:
		case <-ctx.Done():
		default:
			// Relay still holds the previous tick; drop this tick.
		}
	}

	// Expression was already validated in NewCronTrigger, so AddFunc
	// cannot fail here.
	_, _ = c.AddFunc(t.expr, emitter)

	c.Start()
	defer c.Stop()

	// Block until ctx is cancelled.
	<-ctx.Done()
}

// --- EventTrigger ---

// EventTrigger forwards events from an external channel. It is the bridge
// between an event source (e.g. a poller that detects leaks) and the
// scheduler pipeline. The caller owns the source channel and controls its
// lifetime; closing it exhausts the trigger.
type EventTrigger struct {
	ch <-chan Event // source channel owned by the caller
}

// NewEventTrigger creates a Trigger that fires whenever an event arrives on
// ch. Emit returns when ch is closed or ctx is cancelled.
func NewEventTrigger(ch <-chan Event) *EventTrigger {
	return &EventTrigger{ch: ch}
}

// Description returns "event".
func (t *EventTrigger) Description() string {
	return "event"
}

// Emit forwards each Event received from the source channel into ch until
// the source channel is closed or ctx is cancelled. Each forwarded send is
// guarded by ctx.Done so that a cancellation arriving while the relay has
// not yet consumed the previous event does not leak this goroutine.
func (t *EventTrigger) Emit(ctx context.Context, ch chan<- Event) {
	for {
		select {
		case <-ctx.Done():
			return
		case ev, ok := <-t.ch:
			if !ok {
				return
			}
			select {
			case ch <- ev:
			case <-ctx.Done():
				return
			}
		}
	}
}

// --- OnceTrigger ---

// OnceTrigger fires exactly once, immediately when Emit is called. It is
// useful for jobs that should run once at scheduler startup (e.g. an
// initial sync before the periodic interval takes over).
type OnceTrigger struct{}

// NewOnceTrigger creates a Trigger that fires exactly once.
func NewOnceTrigger() *OnceTrigger {
	return &OnceTrigger{}
}

// Description returns "once".
func (t *OnceTrigger) Description() string {
	return "once"
}

// Emit sends a single empty Event on ch and returns. If ctx is cancelled
// before the relay consumes the event, Emit returns without sending.
func (t *OnceTrigger) Emit(ctx context.Context, ch chan<- Event) {
	select {
	case ch <- Event{}:
	case <-ctx.Done():
	}
}
