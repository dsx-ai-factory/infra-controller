// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"testing"
	"testing/synctest"
	"time"
)

func TestWatchdogFiresWhenNotDisarmed(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		fired := make(chan struct{})
		wd := armWatchdog(time.Second, func() { close(fired) })
		time.Sleep(2 * time.Second)
		synctest.Wait()
		select {
		case <-fired:
		default:
			t.Error("the watchdog did not fire past its deadline")
		}
		wd.disarm()
	})
}

func TestWatchdogDoesNotFireWhenDisarmed(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		fired := false
		wd := armWatchdog(time.Second, func() { fired = true })
		wd.disarm()
		time.Sleep(2 * time.Second)
		synctest.Wait()
		if fired {
			t.Error("a disarmed watchdog still fired")
		}
	})
}
