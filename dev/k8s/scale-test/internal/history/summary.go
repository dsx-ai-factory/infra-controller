// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package history

import (
	"context"
	"fmt"
	"math"
	"math/bits"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// When a count crossed each fixed target, and how far off that time can be.
//
// Each crossing is timed at the first sweep end that meets the target. A
// crossing happened at most one sweep gap plus one sweep duration before the
// E that shows it, plus the dispatch window, so the timing error bound is
// the largest sweep gap, the longest sweep and the write window. A stage is
// timed only up from a proven zero: a sweep before the window showing count
// 0. Without it the times are unknown, never read as zero.

// qParts is 10^6: every fixed quantile is a whole number of these.
const qParts = 1_000_000

// maxTime is the last instant RFC 3339 can write.
var maxTime = jsonx.Time{Time: time.Date(9999, 12, 31, 23, 59, 59, 999_999_999, time.UTC)}

// AtSecs is the time of Unix seconds s. Times come from store samples, so any
// value is possible: NaN or one before the epoch is the epoch, one past year
// 9999 is maxTime.
func AtSecs(s float64) jsonx.Time {
	if !(s > 0) {
		return jsonx.Time{Time: time.Unix(0, 0).UTC()}
	}
	if s >= lastTime+1 {
		return maxTime
	}
	whole, frac := math.Modf(s)
	return jsonx.Time{Time: time.Unix(int64(whole), int64(math.Round(frac*1e9))).UTC()}
}

// num is x as a reply value: a -0, which the encoder refuses, becomes 0.
func num(x float64) *float64 {
	x += 0
	return &x
}

// unixSecs is t, at or after the epoch, in Unix seconds.
func unixSecs(t time.Time) float64 { return float64(t.Unix()) + float64(t.Nanosecond())/1e9 }

// input is the request, in seconds.
type input struct {
	base             *probeapi.SummaryBase
	family           probeapi.Family
	start, end       float64
	n                uint64
	addWindowSeconds float64
	// process is the probe process the run's metadata named. The window is
	// bound to it. interval is the family's reader cadence, for the freshness
	// of the baseline.
	process, interval float64
}

// target is ceil(q*n), the count that crosses the fixed quantile q, and
// whether it is a count (at most MaxCount). q is a FixedQuantiles entry, a
// whole number of millionths, so q*n is exact in 128-bit integers.
func target(n uint64, q float64) (uint64, bool) {
	parts := uint64(math.Round(q * qParts))
	hi, lo := bits.Mul64(parts, n)
	d, rem := bits.Div64(hi, lo, qParts)
	if rem > 0 {
		d++
	}
	return d, d <= probeapi.MaxCount
}

// evidence selects the sweeps a summary from start reads, bound to the run's
// process: before is the latest same-process sweep before the window (the
// proven starting count), window is every sweep ending in it. foreign is set
// when another process is observed in the span (baseline, or window start,
// through end) by a sweep end or any raw scrape: every point is checked, not
// only a collapsed first-seen time, so a foreign process re-scraped in the
// span while exporting an old sweep is a fault, and one ending exactly at the
// baseline is not excluded. Observations wholly before the span are ignored.
func evidence(sweeps []sweep, points []point, start, end, process float64) (before *sweep, window []*sweep, foreign bool) {
	pbits := math.Float64bits(process)
	for i := range sweeps {
		if s := &sweeps[i]; s.end < start && math.Float64bits(s.process) == pbits {
			before = s // the latest same-process sweep before the window
		}
	}
	spanStart := start
	if before != nil {
		spanStart = before.end
	}
	inSpan := func(tsec float64) bool { return tsec >= spanStart && tsec <= end }
	for i := range sweeps {
		s := &sweeps[i]
		if s.end >= start && s.end <= end {
			window = append(window, s)
		}
		if math.Float64bits(s.process) != pbits && inSpan(s.end) {
			foreign = true
		}
	}
	for _, p := range points {
		if math.Float64bits(p.process) != pbits && inSpan(float64(p.at)/1000) {
			foreign = true
		}
	}
	return before, window, foreign
}

// crossing is the index of the first sweep of window whose count meets
// target, or -1.
func crossing(window []*sweep, target uint64) int {
	return slices.IndexFunc(window, func(s *sweep) bool { return s.count >= float64(target) })
}

// monotone reports whether the span's counts never fall. A regression breaks
// the progress assumption the crossing times rest on, so the times are then
// unknown.
func monotone(span []*sweep) bool {
	for i := 1; i < len(span); i++ {
		if span[i].count < span[i-1].count {
			return false
		}
	}
	return true
}

func compute(in *input, before *sweep, window []*sweep, finalScrapeSeen bool) probeapi.SummaryResponse {
	span := window
	if before != nil {
		span = append([]*sweep{before}, window...)
	}
	r := probeapi.SummaryResponse{
		Family: in.family, Start: AtSecs(in.start), End: AtSecs(in.end), N: in.n,
		Crossings: make([]probeapi.Crossing, len(probeapi.FixedQuantiles)), AddWindowSeconds: in.addWindowSeconds + 0,
		FinalScrapeSeen: finalScrapeSeen, SweepsSeen: uint32(min(len(window), math.MaxUint32)),
	}
	for i, q := range probeapi.FixedQuantiles {
		t, _ := target(in.n, q) // Prepare checked every target is a count.
		if in.base != nil {
			t += in.base.Count
		}
		r.Crossings[i] = probeapi.Crossing{Q: q, Target: t}
	}
	if len(span) == 0 {
		return r // no usable sweep: every value stays null.
	}
	last := span[len(span)-1]
	r.FinalCount, r.FinalSweepAt = new(uint64(max(last.count, 0))), new(AtSecs(last.end))
	// A stage is timed only from a fresh proven zero: a same-process sweep
	// before the window, showing count 0, no older than two reader intervals
	// at the window start. A missing, stale or non-zero baseline, or a
	// regression anywhere in the span, leaves the times unknown, never read as
	// a clean completion.
	switch {
	case before == nil || before.end < in.start-2*in.interval:
		return r
	case in.base == nil && (before.count != 0 || !monotone(span)):
		return r
	case in.base != nil && (before.count != float64(in.base.Count) || before.start != unixSecs(in.base.SweepStart.Time) || before.end != unixSecs(in.base.ReadAt.Time) || before.process != in.base.ProcessStartSeconds):
		return r
	}
	for i := range r.Crossings {
		c := &r.Crossings[i]
		j := crossing(window, c.Target)
		if j < 0 {
			continue
		}
		s := window[j]
		worst := s.count
		for _, later := range window[j:] {
			worst = min(worst, later.count)
		}
		c.ReachedAt, c.Seconds, c.WorstCountAfter = new(AtSecs(s.end)), num(s.end-in.start), new(uint64(max(worst, 0)))
		if r.WorstCountAfterFirstCrossing == nil {
			r.WorstCountAfterFirstCrossing = c.WorstCountAfter
		}
	}
	// Sweep gaps tile the window: from the sweep before it through every
	// sweep end in it, to its end. The largest gap plus the longest sweep
	// plus the dispatch window bound how long before a sweep end a crossing
	// could have happened. A missed scrape only widens a gap, which the
	// bound already uses.
	maxGap, maxDuration, prev := 0.0, 0.0, before.end
	for _, s := range window {
		maxGap, prev = max(maxGap, s.end-prev), s.end
	}
	maxGap = max(maxGap, in.end-prev)
	for _, s := range span {
		maxDuration = max(maxDuration, s.end-s.start)
	}
	r.MaxSweepGapSeconds, r.MaxSweepDurationSeconds = num(maxGap), num(maxDuration)
	r.TimingErrorSeconds = num(maxGap + maxDuration + in.addWindowSeconds)
	return r
}

// Request is a checked summary request: the series it reads and its window.
// Built by Prepare before any store read, and never changed after.
type Request struct {
	// Spec is the count series, and the capabilities every sweep read must
	// show.
	Spec       probeapi.SeriesSpec
	Start, End time.Time
	in         input
}

// Samples are what a store client read for one Request: the count series,
// the sweep time series of its family, and the flag series of each of its
// needs, in order. A need without flag series has no flag samples, so no
// sweep meets it.
type Samples struct {
	Counts, Times []RawSeries
	Caps          [][]RawSeries
}

// Prepare checks a summary of f over p before any store read: the window
// ends by now, starts after the epoch and spans at most maxWindow, and every
// fixed target is a count.
func Prepare(ctx context.Context, c Context, f probeapi.Filter, p probeapi.SummaryParams, maxWindow time.Duration) (*Request, error) {
	if err := c.Check(ctx); err != nil {
		return nil, err
	}
	// History has no current host read: a compact host field is a need,
	// checked in every sweep read.
	spec, err := probeapi.SeriesSpecFor(f, false)
	if err != nil {
		return nil, Unsupported{err.Error()}
	}
	interval, read := c.Probe.ReaderIntervals[spec.Family]
	if !read {
		return nil, Unsupported{"the probe reads no " + string(spec.Family) + " sweeps"}
	}
	switch {
	case p.End.After(c.Now()):
		return nil, InvalidInput{"end", "end is in the future"}
	case !p.Start.Before(p.End.Time):
		return nil, InvalidInput{"start", "start must be before end"}
	case p.Start.Before(time.Unix(0, 0)):
		return nil, InvalidInput{"start", "start is before the Unix epoch"}
	case p.End.Sub(p.Start.Time) > maxWindow:
		return nil, InvalidInput{"start", fmt.Sprintf("the window is longer than %v", maxWindow)}
	}
	for _, q := range probeapi.FixedQuantiles {
		if target, ok := target(p.N, q); !ok || p.Base != nil && (p.Base.Count > probeapi.MaxCount || target > probeapi.MaxCount-p.Base.Count) {
			return nil, InvalidInput{"n", "a target would exceed the largest count: n is too large"}
		}
	}
	if p.Base != nil && (p.Base.ProcessStartSeconds != c.Probe.Info.ProcessStartSeconds || !p.Base.SweepStart.Before(p.Base.ReadAt.Time) || !p.Base.ReadAt.Before(p.Start.Time)) {
		return nil, InvalidInput{"base", "baseline epoch or source times do not precede the window"}
	}
	// A write window longer than the longest run window is no write window,
	// and a huge one would make the timing error infinite.
	if !(p.AddWindowSeconds >= 0 && p.AddWindowSeconds <= maxWindow.Seconds()) {
		return nil, InvalidInput{"addWindowSeconds", "addWindowSeconds must be from 0 to the longest window"}
	}
	base := p.Base
	if base != nil {
		copy := *base
		base = &copy
	}
	r := &Request{Spec: spec, Start: p.Start.Time, End: p.End.Time, in: input{
		base: base, family: spec.Family, start: unixSecs(p.Start.Time), end: unixSecs(p.End.Time), n: p.N, addWindowSeconds: p.AddWindowSeconds,
		process: c.Probe.Info.ProcessStartSeconds, interval: interval.Seconds(),
	}}
	// A request checked past the deadline, or after a cancel, is not taken.
	if err := c.Check(ctx); err != nil {
		return nil, err
	}
	return r, nil
}

// points joins the samples. A need is met only where its flag is exactly 1.
func (r *Request) points(s Samples) ([]point, error) {
	caps := make([]capSeries, len(r.Spec.Needs))
	for i, need := range r.Spec.Needs {
		c := need.Capability()
		caps[i] = capSeries{name: c.Gauge, label: c.Label, value: c.Value}
		if i < len(s.Caps) {
			caps[i].series = s.Caps[i]
		}
	}
	return join(s.Counts, probeapi.GaugeLabels(r.Spec.Metric), s.Times, caps)
}

// finalScrape reports whether points hold a whole scrape of the run's
// process at or after the request's end. The scrape instant is compared with
// End directly, so a scrape stamped at the millisecond before an end with a
// sub-millisecond fraction does not confirm it, and the comparison holds
// across the whole accepted time range, past the UnixNano limit.
func (r *Request) finalScrape(points []point) bool {
	pbits := math.Float64bits(r.in.process)
	return slices.ContainsFunc(points, func(p point) bool {
		return !time.UnixMilli(p.at).Before(r.End) && math.Float64bits(p.process) == pbits
	})
}

// FinalScrapeSeen reports whether s holds a whole scrape at or after the
// request's end. Until one lands, sweeps that ended before the end may not
// have been scraped yet: a store client polls for it.
func FinalScrapeSeen(ctx context.Context, c Context, r *Request, s Samples) (bool, error) {
	if err := c.Check(ctx); err != nil {
		return false, err
	}
	points, err := r.points(s)
	if err != nil {
		return false, err
	}
	// A reply made past the deadline, or after a cancel, is not taken.
	if err := c.Check(ctx); err != nil {
		return false, err
	}
	return r.finalScrape(points), nil
}

// Summarize is the summary of r from the samples s read for it. Every sweep
// the summary reads must show the capabilities its filter needs: a missing or
// false flag in any of them is Unavailable, never proof.
func Summarize(ctx context.Context, c Context, r *Request, s Samples) (probeapi.SummaryResponse, error) {
	var none probeapi.SummaryResponse
	if err := c.Check(ctx); err != nil {
		return none, err
	}
	points, err := r.points(s)
	if err != nil {
		return none, err
	}
	sweeps, err := sweepsOf(points)
	if err != nil {
		return none, err
	}
	if err := c.Check(ctx); err != nil {
		return none, err
	}
	before, window, foreign := evidence(sweeps, points, r.in.start, r.in.end, r.in.process)
	if foreign {
		return none, Unavailable{"a sweep in the window is from another probe process: a restart or a replaced pod"}
	}
	if r.in.base != nil {
		before = plannedBaseline(sweeps, points, &r.in)
	}
	span := window
	if before != nil {
		span = append([]*sweep{before}, window...)
	}
	if len(r.Spec.Needs) > 0 && slices.ContainsFunc(span, func(s *sweep) bool { return s.capable != yes }) {
		return none, Unavailable{"a sweep in the window could not answer the allocatable host filter"}
	}
	reply := compute(&r.in, before, window, r.finalScrape(points))
	// A reply made past the deadline, or after a cancel, is not taken.
	if err := c.Check(ctx); err != nil {
		return none, err
	}
	return reply, nil
}

// plannedBaseline proves the frozen source reference, allowing intervening
// pre-dispatch sweeps only while their count and process remain unchanged.
func plannedBaseline(sweeps []sweep, points []point, in *input) *sweep {
	var found *sweep
	b := in.base
	for i := range sweeps {
		s := &sweeps[i]
		if s.process == b.ProcessStartSeconds && s.start == unixSecs(b.SweepStart.Time) && s.end == unixSecs(b.ReadAt.Time) && s.count == float64(b.Count) {
			found = s
		}
	}
	if found == nil {
		return nil
	}
	for _, s := range sweeps {
		if s.end >= found.end && s.end < in.start && (s.process != in.process || s.count != float64(b.Count)) {
			return nil
		}
	}
	for _, p := range points {
		at := float64(p.at) / 1000
		if at >= found.end && at <= in.end && p.process != in.process {
			return nil
		}
	}
	return found
}
