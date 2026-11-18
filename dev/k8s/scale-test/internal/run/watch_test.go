// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
)

func TestWatchGateLossCancelsRun(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctl := NewControl()
		gate := lock.Open()
		w := SpawnWatch(ctl, gate, RealClock{}, context.Background())
		gate.Close()
		time.Sleep(gateTick + time.Second)
		synctest.Wait()
		if w.LostAt() == nil {
			t.Error("the gate loss time was not recorded")
		}
		if !ctl.Cancelled() {
			t.Error("a lost lock must cancel the run")
		}
		w.Stop()
	})
}

func TestWatchOutsideCancelCancelsRun(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctl := NewControl()
		outside, cancel := context.WithCancel(context.Background())
		w := SpawnWatch(ctl, lock.Open(), RealClock{}, outside)
		cancel()
		synctest.Wait()
		if !ctl.Cancelled() {
			t.Error("an outside cancel must cancel the run")
		}
		w.Stop()
	})
}

// TestWatchSealedGateIsNotLost pins the seal-versus-lost boundary: a tick after a seal (while the Lease is still
// writable) records no loss and never cancels, but an actual closure still does. The watch reads LeaseWritable, not Check.
func TestWatchSealedGateIsNotLost(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctl := NewControl()
		gate := lock.Open()
		w := SpawnWatch(ctl, gate, RealClock{}, context.Background())
		gate.Seal() // sealed for teardown, but the holder's Lease stays writable
		time.Sleep(3 * gateTick)
		synctest.Wait()
		if w.LostAt() != nil {
			t.Error("a teardown seal was recorded as a lost lock")
		}
		if ctl.Cancelled() {
			t.Error("a teardown seal cancelled the run")
		}
		// The same gate, once genuinely closed, is a loss.
		gate.Close()
		time.Sleep(gateTick + time.Second)
		synctest.Wait()
		if w.LostAt() == nil || !ctl.Cancelled() {
			t.Error("an actual gate closure must still record a loss and cancel")
		}
		w.Stop()
	})
}
