// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"testing"
	"time"
)

// TestDeriveBoundsAreOrdered proves every bound derives from the config timeouts and the start: the wall deadline is the
// window, the evidence end the whole-run time, the probe read end a finalize margin before it, the finalize end the evidence end.
func TestDeriveBoundsAreOrdered(t *testing.T) {
	b := Derive(cfg(t.TempDir()), RealClock{}) // whole 300, finalize 120
	if d := b.Deadline.Sub(b.Started); d != 180*time.Second {
		t.Errorf("wall window = %v, want 180s", d)
	}
	if d := b.EvidenceEnd.Sub(b.StartedAt); d != 300*time.Second {
		t.Errorf("evidence end = %v past start, want 300s (deadline + finalize)", d)
	}
	if d := b.EvidenceEnd.Sub(b.ProbeReadEnd); d != FinalizeMargin {
		t.Errorf("probe read end is %v before the evidence end, want %v", d, FinalizeMargin)
	}
	if !b.FinalizeEnd.Equal(b.EvidenceEnd) {
		t.Error("the finalize end must be the evidence end")
	}
}

func TestControlSoftStopVersusCancel(t *testing.T) {
	c := NewControl()
	c.RequestStop()
	select {
	case <-c.Stopper().Stopped():
	default:
		t.Error("a soft stop must close the stopper")
	}
	if c.Cancelled() {
		t.Error("a soft stop must not cancel the run context")
	}
	if !c.StopRequested() {
		t.Error("StopRequested must be true after a soft stop")
	}

	hard := NewControl()
	hard.Cancel()
	if !hard.Cancelled() {
		t.Error("a cancel must end the run context")
	}
	select {
	case <-hard.Stopper().Stopped():
	default:
		t.Error("a cancel must also close the stopper")
	}
	if hard.StopRequested() {
		t.Error("a cancel is not a soft-stop request")
	}
}
