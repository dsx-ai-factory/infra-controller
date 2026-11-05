// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"sync"
	"sync/atomic"
	"time"
)

// gateTick is how often the watch checks the lock gate.
const gateTick = time.Second

// gateState reports whether the holder's Lease is still writable. The watch reads this, not Check: Check also fails on
// a teardown seal, so an in-flight tick just after the seal would record a false loss. LeaseWritable ignores the seal.
type gateState interface{ LeaseWritable() bool }

// Watch watches the lock gate and the outside cancel. A lost Lease records the loss time and cancels the run; a
// cancel from outside cancels the run. The two run in independent goroutines, so neither blocks the other nor shutdown.
// The control-ConfigMap soft-stop surface is deferred for v1; the soft-stop primitive (Control.RequestStop) stays.
type Watch struct {
	cancel context.CancelFunc
	done   chan struct{}
	lostAt atomic.Pointer[time.Time]
}

// SpawnWatch starts the watch: gate is the held lock's write gate, outside is the outside cancel.
func SpawnWatch(ctl *Control, gate gateState, clock Clock, outside context.Context) *Watch {
	wctx, cancel := context.WithCancel(context.Background())
	w := &Watch{cancel: cancel, done: make(chan struct{})}
	var wg sync.WaitGroup
	wg.Add(2)
	go func() { // the gate, on its own schedule
		defer wg.Done()
		tick := time.NewTicker(gateTick)
		defer tick.Stop()
		for {
			select {
			case <-wctx.Done():
				return
			case <-tick.C:
			}
			if !gate.LeaseWritable() {
				now := clock.Now()
				w.lostAt.CompareAndSwap(nil, &now)
				ctl.Cancel()
				return
			}
		}
	}()
	go func() { // a cancel from outside
		defer wg.Done()
		select {
		case <-wctx.Done():
		case <-outside.Done():
			ctl.Cancel()
		}
	}()
	go func() { wg.Wait(); close(w.done) }()
	return w
}

// Stop ends the watch and waits for its goroutines.
func (w *Watch) Stop() {
	w.cancel()
	<-w.done
}

// LostAt is the wall time the gate was first seen closed, or nil.
func (w *Watch) LostAt() *time.Time { return w.lostAt.Load() }
