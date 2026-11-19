// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"sync"
	"time"
)

// Drain is how long a run cancelled at the evidence end has to stop; one that does not is dropped, its writes in flight stay unclear.
const Drain = 60 * time.Second

// ShutdownBudget is the cap each watchdog enforces: the whole-run end plus it is the absolute maximum; a cancel or
// teardown entry caps the rest at it. So neither a startup hang nor a stuck teardown can sit past the Job's grace.
const ShutdownBudget = 90 * time.Second

// CleanupBudget bounds the detached publication and release so an outside cancel cannot stop the run recording its own
// outcome and cleanup state. It sits under the shutdown watchdogs.
const CleanupBudget = 60 * time.Second

// watchdog is the absolute shutdown deadline. On expiry its action runs: the process exits so a stuck join cannot hang.
type watchdog struct {
	stop chan struct{}
	once sync.Once
}

func armWatchdog(d time.Duration, onExpire func()) *watchdog {
	wd := &watchdog{stop: make(chan struct{})}
	go func() {
		t := time.NewTimer(d)
		defer t.Stop()
		select {
		case <-t.C:
			onExpire()
		case <-wd.stop:
		}
	}()
	return wd
}

func (wd *watchdog) disarm() { wd.once.Do(func() { close(wd.stop) }) }

// armOnCancel caps the execution at budget once ctx is canceled. It waits for the cancel on its own goroutine, so it
// fires even while the work is blocked in a context-ignoring call, then runs onExpire after budget. disarm stops it.
func armOnCancel(ctx context.Context, budget time.Duration, onExpire func()) *watchdog {
	wd := &watchdog{stop: make(chan struct{})}
	go func() {
		select {
		case <-wd.stop:
			return
		case <-ctx.Done():
		}
		t := time.NewTimer(budget)
		defer t.Stop()
		select {
		case <-t.C:
			onExpire()
		case <-wd.stop:
		}
	}()
	return wd
}
