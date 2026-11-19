// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"sync"
	"sync/atomic"
)

// Stopper is the run's soft-stop signal (a writer.Stopper): its channel closes once, on a soft stop or a cancel.
type Stopper struct {
	ch   chan struct{}
	once sync.Once
}

func newStopper() *Stopper { return &Stopper{ch: make(chan struct{})} }

// Stopped is the channel the writer watches.
func (s *Stopper) Stopped() <-chan struct{} { return s.ch }

func (s *Stopper) stop() { s.once.Do(func() { close(s.ch) }) }

// Control is the run's own cancel, separate from the outside-cancel context. A soft stop lets work in flight finish
// and dispatches nothing new; a cancel also ends the run context. Both close the Stopper.
type Control struct {
	ctx      context.Context
	cancel   context.CancelFunc
	stopper  *Stopper
	softStop atomic.Bool
}

// NewControl makes a run control with its own cancellable context.
func NewControl() *Control {
	ctx, cancel := context.WithCancel(context.Background())
	return &Control{ctx: ctx, cancel: cancel, stopper: newStopper()}
}

// Context is the run context, cancelled by Cancel or the watch.
func (c *Control) Context() context.Context { return c.ctx }

// Stopper is the writer's soft-stop channel.
func (c *Control) Stopper() *Stopper { return c.stopper }

// RequestStop asks for a soft stop: the Stopper closes, the context stays live.
func (c *Control) RequestStop() {
	c.softStop.Store(true)
	c.stopper.stop()
}

// Cancel ends the run context and closes the Stopper: a hard stop.
func (c *Control) Cancel() {
	c.cancel()
	c.stopper.stop()
}

// StopRequested is true once a soft stop was asked for.
func (c *Control) StopRequested() bool { return c.softStop.Load() }

// Cancelled is true once the run context ended.
func (c *Control) Cancelled() bool { return c.ctx.Err() != nil }
