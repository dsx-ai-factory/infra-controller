// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store_test

import (
	"fmt"
	"math"
	"slices"
	"strconv"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// crossingsMatch compares a summary's crossings with the operator's independent
// quantiles, targets and raw-curve times. It requires exactly one crossing per
// fixed quantile and one answer each, so an empty, short or extra result cannot
// pass unchecked. Times match to timestamp precision (slack).
func crossingsMatch(crossings []probeapi.Crossing, targets, reached []string, slack time.Duration) error {
	q := len(probeapi.FixedQuantiles)
	if len(crossings) != q || len(targets) != q || len(reached) != q {
		return fmt.Errorf("want %d crossings and answers, got %d crossings, %d targets, %d times",
			q, len(crossings), len(targets), len(reached))
	}
	for i, c := range crossings {
		wantT, terr := strconv.ParseUint(targets[i], 10, 64)
		wantR, rerr := time.Parse(time.RFC3339, reached[i])
		switch {
		case terr != nil || rerr != nil:
			return fmt.Errorf("answer %d: target %v time %v", i, terr, rerr)
		case c.Q != probeapi.FixedQuantiles[i]:
			return fmt.Errorf("crossing %d quantile: got %v, want %v", i, c.Q, probeapi.FixedQuantiles[i])
		case c.Target != wantT:
			return fmt.Errorf("crossing %d target: got %d, want %d", i, c.Target, wantT)
		case c.ReachedAt == nil:
			return fmt.Errorf("crossing %d has no time", i)
		case c.ReachedAt.Sub(wantR) < -slack || c.ReachedAt.Sub(wantR) > slack:
			return fmt.Errorf("crossing %d reached %v, want %v within %v", i, c.ReachedAt.Time, wantR, slack)
		}
	}
	return nil
}

// TestCrossingsMatch proves a short, extra or empty crossing list, a short
// answer list, a wrong target/quantile and an off-precision time all fail.
func TestCrossingsMatch(t *testing.T) {
	q := len(probeapi.FixedQuantiles)
	origin := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	var cr []probeapi.Crossing
	var tg, rc []string
	for i := range q {
		at := jsonx.Time{Time: origin.Add(time.Duration(i) * time.Minute)}
		cr = append(cr, probeapi.Crossing{Q: probeapi.FixedQuantiles[i], Target: uint64(i + 1), ReachedAt: &at})
		tg, rc = append(tg, strconv.Itoa(i+1)), append(rc, at.Format(time.RFC3339))
	}
	mutate := func(f func([]probeapi.Crossing)) []probeapi.Crossing { c := slices.Clone(cr); f(c); return c }
	late := jsonx.Time{Time: origin.Add(time.Hour)}
	for _, c := range []struct {
		name             string
		crossings        []probeapi.Crossing
		targets, reached []string
		ok               bool
	}{
		{"an exact in-precision set", cr, tg, rc, true},
		{"short crossings", cr[:q-1], tg, rc, false},
		{"extra crossing", append(slices.Clone(cr), cr[0]), tg, rc, false},
		{"empty crossings", nil, tg, rc, false},
		{"short targets", cr, tg[:q-1], rc, false},
		{"wrong target", mutate(func(c []probeapi.Crossing) { c[0].Target = 999 }), tg, rc, false},
		{"time outside precision", mutate(func(c []probeapi.Crossing) { c[2].ReachedAt = &late }), tg, rc, false},
		{"wrong quantile", mutate(func(c []probeapi.Crossing) { c[0].Q = 0.123 }), tg, rc, false},
		{"duplicate quantile", mutate(func(c []probeapi.Crossing) { c[1].Q = probeapi.FixedQuantiles[0] }), tg, rc, false},
	} {
		if err := crossingsMatch(c.crossings, c.targets, c.reached, time.Second); (err == nil) != c.ok {
			t.Errorf("%s: ok=%v err=%v", c.name, c.ok, err)
		}
	}
}

// checkTimingError requires a finite positive minimum and a finite positive
// reported error at or above it: NaN, infinite, zero, negative or below fails.
func checkTimingError(got *float64, minError float64) error {
	switch {
	case !(minError > 0) || math.IsInf(minError, 0):
		return fmt.Errorf("the timing-error minimum %v must be finite and positive", minError)
	case got == nil:
		return fmt.Errorf("the summary reports no timing error")
	case !(*got > 0) || math.IsInf(*got, 0):
		return fmt.Errorf("the reported timing error %v must be finite and positive", *got)
	case *got < minError:
		return fmt.Errorf("the reported timing error %v is under the minimum %v", *got, minError)
	}
	return nil
}

// TestCheckTimingError proves a nil, zero, NaN, infinite or negative value or
// floor, and a below-floor value, all fail; at or above a positive floor passes.
func TestCheckTimingError(t *testing.T) {
	f := func(v float64) *float64 { return &v }
	for _, c := range []struct {
		name string
		got  *float64
		min  float64
		ok   bool
	}{
		{"nil reported", nil, 1, false},
		{"zero reported", f(0), 1, false},
		{"negative reported", f(-1), 1, false},
		{"NaN reported", f(math.NaN()), 1, false},
		{"Inf reported", f(math.Inf(1)), 1, false},
		{"NaN floor", f(2), math.NaN(), false},
		{"zero floor", f(2), 0, false},
		{"negative floor", f(2), -1, false},
		{"Inf floor", f(2), math.Inf(1), false},
		{"below floor", f(1), 2, false},
		{"at floor", f(2), 2, true},
		{"above floor", f(3), 2, true},
	} {
		if err := checkTimingError(c.got, c.min); (err == nil) != c.ok {
			t.Errorf("%s: ok=%v err=%v", c.name, c.ok, err)
		}
	}
}
