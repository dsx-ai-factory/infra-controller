// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package run is the run-control skeleton: the bounded clocks and contexts, the site-lock and control watch, the
// drain and shutdown watchdog, and the claim-and-publish of the run's result. The test commands plug into it.
package run

import "time"

// Clock is the run-wide time source: Now is the wall time the result records, Instant a monotonic reading for deadlines.
// A run shares one. Tests run under testing/synctest, whose fake clock makes the real clock deterministic.
type Clock interface {
	Now() time.Time
	Instant() time.Time
}

// RealClock is the production clock. time.Now carries a monotonic reading, so Instant measures elapsed time.
type RealClock struct{}

func (RealClock) Now() time.Time     { return time.Now() }
func (RealClock) Instant() time.Time { return time.Now() }
