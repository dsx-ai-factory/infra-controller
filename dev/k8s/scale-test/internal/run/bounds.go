// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
)

// FinalizeMargin is how far the probe read end sits before the evidence end, so a last read and the finalize have room.
const FinalizeMargin = 60 * time.Second

// Bounds are the run's deadlines, all derived once from the config timeouts and the start. The wall Deadline goes in
// the result; the monotonic instants bound the work.
type Bounds struct {
	Started      time.Time // the wall start
	StartedAt    time.Time // the monotonic start
	Deadline     time.Time // the wall end of the test window: started + wholeRun - finalize
	EvidenceEnd  time.Time // the monotonic end of the run and end capture: the deadline plus the finalize budget
	ProbeReadEnd time.Time // EvidenceEnd - FinalizeMargin: the last probe read
	FinalizeEnd  time.Time // the monotonic end of the finalize budget: the evidence end
}

// Derive computes the run's bounds from the config timeouts and the clock. Config guarantees readiness < finalize < wholeRun.
func Derive(c *config.Config, clock Clock) Bounds {
	wholeRun := time.Duration(c.Timeouts.WholeRunSeconds) * time.Second
	finalize := time.Duration(c.Timeouts.FinalizeSeconds) * time.Second
	started, startedAt := clock.Now(), clock.Instant()
	evidenceEnd := startedAt.Add(wholeRun)
	return Bounds{
		Started: started, StartedAt: startedAt, Deadline: started.Add(wholeRun - finalize),
		EvidenceEnd: evidenceEnd, ProbeReadEnd: evidenceEnd.Add(-FinalizeMargin), FinalizeEnd: evidenceEnd,
	}
}
