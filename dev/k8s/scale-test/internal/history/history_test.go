// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package history

import (
	"context"
	"errors"
	"fmt"
	"math"
	"reflect"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

const (
	t0 = 1_000_000.0
	// testProcess is the probe process every fixture scrapes under, the one
	// info() names in the metadata the summary binds to.
	testProcess = t0 - 100_000
	day         = 24 * time.Hour
	units       = probeapi.Prefix + "units"
	hosts       = probeapi.Prefix + "hosts"
	known       = probeapi.Prefix + "host_predicates_known"
)

var (
	ready      = probeapi.Filter{Type: probeapi.UnitTypeHost, Stage: new(probeapi.StageReady)}
	readyAlloc = probeapi.Filter{Type: probeapi.UnitTypeHost, Stage: new(probeapi.StageReady), Allocatable: new(true)}
	maxCount   = float64(probeapi.MaxCount)
)

func eq(t *testing.T, got, want any, what ...any) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%v: got %v, want %v", what, got, want)
	}
}

func isBadStore(err error) bool {
	var u Unavailable
	return errors.As(err, &u) && strings.HasPrefix(u.Text, "the stored series are not the probe's")
}

func isUnavailable(err error) bool { return errors.As(err, new(Unavailable)) }

// opt is *p, or - for nil.
func opt[T any](p *T) string {
	if p == nil {
		return "-"
	}
	return fmt.Sprint(*p)
}

// reached reports whether the first crossing was reached.
func reached(r probeapi.SummaryResponse) bool {
	return len(r.Crossings) > 0 && r.Crossings[0].ReachedAt != nil
}

// crossed is each crossing as target@seconds/worstCountAfter.
func crossed(r probeapi.SummaryResponse) string {
	var out []string
	for _, c := range r.Crossings {
		out = append(out, fmt.Sprintf("%d@%s/%s", c.Target, opt(c.Seconds), opt(c.WorstCountAfter)))
	}
	return strings.Join(out, " ")
}

// sw is a sweep of testProcess that ended at end with count, over a 10 s sweep.
func sw(end, count float64) sweep {
	return sweep{testProcess, end - 10, end, count, int64((end + 2) * 1000), yes}
}

// ramp is sweeps every 20 s from t0, counting f(i).
func ramp(n int, f func(int) float64) []sweep {
	out := make([]sweep, n)
	for i := range out {
		out[i] = sw(t0+20*float64(i), f(i))
	}
	return out
}

// in is a host request over [start, end] for n units, bound to testProcess.
func in(start, end float64, n uint64) *input {
	return &input{family: probeapi.FamilyHost, start: start, end: end, n: n, process: testProcess, interval: 20}
}

// withBase prepends a fresh baseline sweep ending just before the window.
func withBase(c float64, window ...sweep) []sweep {
	return append([]sweep{sw(t0-10, c)}, window...)
}

// computed selects the evidence of sweeps bound to in.process and computes
// the summary, as Summarize does.
func computed(in *input, sweeps []sweep, finalScrapeSeen bool) probeapi.SummaryResponse {
	before, window, _ := evidence(sweeps, nil, in.start, in.end, in.process)
	return compute(in, before, window, finalScrapeSeen)
}

func TestCompute(t *testing.T) {
	// n = 18: the fixed targets are ceil(q*18) = 9, 17, 18, 18, 18, 18.
	full := ramp(19, func(i int) float64 { return float64(i) }) // 0..18 every 20 s from t0
	for _, c := range []struct {
		name   string
		sweeps []sweep
		in     *input
		final  bool
		check  func(t *testing.T, r probeapi.SummaryResponse)
	}{
		{"up from a proven zero", withBase(0, full...), in(t0-5, t0+400, 18), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				// 9 at i=9 (t0+180, 185 s from start), 17 at i=17 (345 s), 18 at i=18 (365 s).
				eq(t, crossed(r), "9@185/9 17@345/17 18@365/18 18@365/18 18@365/18 18@365/18")
				eq(t, []any{*r.FinalCount, r.SweepsSeen, r.FinalScrapeSeen}, []any{uint64(18), uint32(19), true})
			}},
		{"a missing baseline leaves the times unknown", full, in(t0-5, t0+400, 18), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, []string{opt(r.Crossings[0].ReachedAt), opt(r.TimingErrorSeconds), opt(r.FinalCount)}, []string{"-", "-", "18"})
			}},
		{"a non-zero baseline leaves the times unknown", withBase(2, full...), in(t0-5, t0+400, 18), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, []any{reached(r), opt(r.TimingErrorSeconds)}, []any{false, "-"})
			}},
		// A stale zero, two reader intervals or more before the start, cannot
		// supply times even though it is zero.
		{"a stale zero baseline leaves the times unknown", append([]sweep{sw(t0-100, 0)}, full...), in(t0-5, t0+400, 18), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, []any{reached(r), opt(r.TimingErrorSeconds), opt(r.FinalCount)}, []any{false, "-", "18"})
			}},
		// A stale zero then a newer non-zero baseline: the nearer baseline is
		// non-zero, so no times.
		{"a stale zero then a newer non-zero baseline is unknown", append([]sweep{sw(t0-100, 0), sw(t0-20, 3)}, full...), in(t0-5, t0+400, 18), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, reached(r), false)
			}},
		// A regression anywhere in the span (0 then 3, 2, 10 for N=10) breaks
		// monotone progress, so the times are unknown, not a clean completion.
		{"a regression leaves the times unknown", withBase(0, sw(t0+20, 3), sw(t0+40, 2), sw(t0+60, 10)), in(t0, t0+100, 10), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, []any{reached(r), opt(r.TimingErrorSeconds), opt(r.FinalCount)}, []any{false, "-", "10"})
			}},
		{"timing error is the gap, the longest sweep and the write window",
			func() []sweep { s := withBase(0, sw(t0+10, 9), sw(t0+70, 18)); s[2].start = t0 + 5; return s }(),
			func() *input { i := in(t0, t0+100, 18); i.addWindowSeconds = 12; return i }(), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				// Gaps over edges t0-10, t0+10, t0+70, t0+100: max 60. Longest
				// sweep is t0+5..t0+70 = 65. 60 + 65 + 12 = 137.
				eq(t, []any{*r.MaxSweepGapSeconds, *r.MaxSweepDurationSeconds, *r.TimingErrorSeconds, r.AddWindowSeconds},
					[]any{60.0, 65.0, 137.0, 12.0})
			}},
		{"no usable sweep is null", []sweep{sw(t0+500, 18)}, in(t0, t0+100, 18), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, []string{opt(r.FinalCount), opt(r.TimingErrorSeconds)}, []string{"-", "-"})
				eq(t, r.Crossings[0].Target, uint64(9))
			}},
		{"negative zeros are written as 0", []sweep{sw(-9, 0), sw(1, 1)},
			func() *input { i := in(0, 100, 1); i.addWindowSeconds = math.Copysign(0, -1); return i }(), true,
			func(t *testing.T, r probeapi.SummaryResponse) {
				eq(t, math.Signbit(r.AddWindowSeconds), false, "-0 add window")
			}},
	} {
		t.Run(c.name, func(t *testing.T) { c.check(t, computed(c.in, c.sweeps, c.final)) })
	}
}

func TestTarget(t *testing.T) {
	// Every fixed target is the exact integer ceiling of q*n over a range of n.
	for _, q := range probeapi.FixedQuantiles {
		parts := uint64(math.Round(q * qParts))
		for n := range uint64(20_001) {
			got, ok := target(n, q)
			if !ok || got != (parts*n+qParts-1)/qParts {
				t.Fatalf("q=%v n=%d: %d", q, n, got)
			}
		}
	}
	if _, ok := target(probeapi.MaxCount, 1); !ok {
		t.Error("n = 2^53 up to q=1 is a count")
	}
	if _, ok := target(probeapi.MaxCount+1, 1); ok {
		t.Error("n = 2^53 + 1 up to q=1 is no count")
	}
}

// store holds samples as a store client merges its replies.
type store map[string]*RawSeries

func (st store) add(name string, at int64, v float64, kv ...string) {
	labels := map[string]string{"__name__": name}
	for i := 0; i < len(kv); i += 2 {
		labels[kv[i]] = kv[i+1]
	}
	k := fmt.Sprint(labels)
	if st[k] == nil {
		st[k] = &RawSeries{Labels: labels}
	}
	st[k].Samples = append(st[k].Samples, Sample{at, v})
}

// samples is what a request counting count reads, one flag series per cap.
func (st store) samples(count string, caps ...string) Samples {
	s := Samples{Caps: make([][]RawSeries, len(caps))}
	for _, x := range st {
		name := x.Labels["__name__"]
		if name == count {
			s.Counts = append(s.Counts, *x)
		}
		if name == sweepEnd || name == sweepStart || name == processStart {
			s.Times = append(s.Times, *x)
		}
		for i, c := range caps {
			if name == c {
				s.Caps[i] = append(s.Caps[i], *x)
			}
		}
	}
	return s
}

// scrapeOf records one scrape of a ready host count from the source labels
// src, showing a host sweep that ended at end for the process start.
func (st store) scrapeOf(src []string, at int64, count, end, process float64) {
	st.add(units, at, count, append([]string{"type", "host", "stage", "ready", "state", "Ready"}, src...)...)
	st.add(sweepEnd, at, end, append([]string{"reader", "devices", "family", "host"}, src...)...)
	st.add(sweepStart, at, end-10, append([]string{"reader", "devices", "family", "host"}, src...)...)
	st.add(processStart, at, process, src...)
}

var selector = []string{"job", "j", "namespace", "ns"}

// ms is the site time offset seconds from t0, in Unix milliseconds.
func ms(offset float64) int64 { return int64(math.Round((t0 + offset) * 1000)) }

// scrape records one scrape at offset at, its host sweep ending at offset end.
func (st store) scrape(at, count, end float64) {
	st.scrapeOf(selector, ms(at), count, t0+end, testProcess)
}

// scrapeAlloc scrapes the hosts gauge with count Ready-and-allocatable hosts and the predicate flag unless NaN.
func (st store) scrapeAlloc(at, count, end, flag float64) {
	st.add(hosts, ms(at), count, append([]string{"state", "Ready", "allocatable", "true"}, selector...)...)
	st.add(sweepEnd, ms(at), t0+end, append([]string{"reader", "devices", "family", "host"}, selector...)...)
	st.add(sweepStart, ms(at), t0+end-10, append([]string{"reader", "devices", "family", "host"}, selector...)...)
	st.add(processStart, ms(at), testProcess, selector...)
	if !math.IsNaN(flag) {
		st.add(known, ms(at), flag, append([]string{"predicate", "allocatable"}, selector...)...)
	}
}

func info() probeapi.MetricsInfo {
	return probeapi.MetricsInfo{
		ContractVersion: probeapi.ContractVersion, Selector: map[string]string{"job": "j", "namespace": "ns"},
		ScrapeIntervalSeconds:  5,
		ReaderIntervalsSeconds: map[probeapi.Family]float64{probeapi.FamilyHost: 20},
		ProcessStartSeconds:    testProcess,
	}
}

// testCtx is a context whose wall clock stands at now, with an hour to run.
func testCtx(now float64) Context {
	probe, _ := NewProbeMetadata(info())
	return Context{Probe: probe, Now: func() time.Time { return AtSecs(now).Time }, Until: time.Now().Add(time.Hour)}
}

func params(start, end float64, n uint64) probeapi.SummaryParams {
	return probeapi.SummaryParams{Start: AtSecs(t0 + start), End: AtSecs(t0 + end), N: n}
}

// summarize prepares and summarizes f over p on the site at t0.
func summarize(f probeapi.Filter, p probeapi.SummaryParams, s Samples) (probeapi.SummaryResponse, error) {
	r, err := Prepare(context.Background(), testCtx(t0), f, p, day)
	if err != nil {
		return probeapi.SummaryResponse{}, err
	}
	return Summarize(context.Background(), testCtx(t0), r, s)
}

func TestProbeMetadata(t *testing.T) {
	m, err := NewProbeMetadata(info())
	eq(t, []any{err, m.ScrapeInterval, m.ReaderIntervals[probeapi.FamilyHost], m.Info}, []any{nil, 5 * time.Second, 20 * time.Second, info()})
	_, read := m.ReaderIntervals[probeapi.FamilyDpu]
	eq(t, read, false, "dpu is not read")
	for _, c := range []struct {
		name  string
		edit  func(*probeapi.MetricsInfo)
		valid bool
	}{
		{"a long reader", func(i *probeapi.MetricsInfo) {
			i.ReaderIntervalsSeconds[probeapi.FamilyHost], i.ScrapeIntervalSeconds = 7_200, 3_600.5
		}, true},
		{"contract", func(i *probeapi.MetricsInfo) { i.ContractVersion++ }, false},
		{"empty selector", func(i *probeapi.MetricsInfo) { clear(i.Selector) }, false},
		{"intrinsic label", func(i *probeapi.MetricsInfo) { i.Selector["family"] = "x" }, false},
		{"zero scrape", func(i *probeapi.MetricsInfo) { i.ScrapeIntervalSeconds = 0 }, false},
		{"negative scrape", func(i *probeapi.MetricsInfo) { i.ScrapeIntervalSeconds = -5 }, false},
		{"NaN scrape", func(i *probeapi.MetricsInfo) { i.ScrapeIntervalSeconds = math.NaN() }, false},
		{"unrepresentable scrape", func(i *probeapi.MetricsInfo) { i.ScrapeIntervalSeconds = 1e300 }, false},
		{"infinite reader", func(i *probeapi.MetricsInfo) { i.ReaderIntervalsSeconds[probeapi.FamilyDpu] = math.Inf(1) }, false},
		{"process before the epoch", func(i *probeapi.MetricsInfo) { i.ProcessStartSeconds = -1 }, false},
		{"NaN process", func(i *probeapi.MetricsInfo) { i.ProcessStartSeconds = math.NaN() }, false},
	} {
		i := info()
		c.edit(&i)
		_, err := NewProbeMetadata(i)
		var u Unsupported
		if c.valid {
			eq(t, err, nil, c.name)
		} else if !errors.As(err, &u) || strings.Contains(u.Text, "300") || strings.Contains(u.Text, "Inf") {
			t.Errorf("%s: %v", c.name, err)
		}
	}
}

func TestEveryCallStopsAtCancelThenDeadline(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		st := store{}
		st.scrape(-20, 0, -25)
		st.scrape(0, 1, -5)
		s := st.samples(units)
		ctx, cancel := context.WithCancel(t.Context())
		c := testCtx(t0)
		c.Until = time.Now().Add(5 * time.Second)
		r, err := Prepare(ctx, c, ready, params(-60, 0, 1), day)
		_, errSummary := Summarize(ctx, c, r, s)
		final, errFinal := FinalScrapeSeen(ctx, c, r, s)
		eq(t, []any{err, errSummary, errFinal, final, c.Check(ctx)}, []any{nil, nil, nil, true, nil}, "in time")
		calls := func() []error {
			_, errPrepare := Prepare(ctx, c, ready, params(-60, 0, 1), day)
			_, errSummary := Summarize(ctx, c, r, s)
			_, errFinal := FinalScrapeSeen(ctx, c, r, s)
			return []error{c.Check(ctx), errPrepare, errSummary, errFinal}
		}
		time.Sleep(5 * time.Second)
		eq(t, calls(), []error{Timeout{}, Timeout{}, Timeout{}, Timeout{}}, "at the deadline")
		cancel()
		eq(t, calls(), []error{Cancelled{}, Cancelled{}, Cancelled{}, Cancelled{}}, "cancel first")

		// The clock is read after the first check: a cancel then still stops it.
		ctx, cancel = context.WithCancel(t.Context())
		c = testCtx(t0)
		c.Now = func() time.Time { cancel(); return AtSecs(t0).Time }
		_, err = Prepare(ctx, c, ready, params(-60, 0, 1), day)
		eq(t, err, error(Cancelled{}), "cancel while prepare reads the clock")
	})
}

func TestJoinAndSweeps(t *testing.T) {
	dims := []string{"type", "stage", "state"}
	src := func(instance string) []string { return []string{"instance", instance, "job", "j"} }
	joined := func(sources map[string][][4]float64) ([]point, []sweep, error) {
		st := store{}
		for instance, rows := range sources {
			for _, r := range rows {
				st.scrapeOf(src(instance), int64(r[0]), r[1], r[2], r[3])
			}
		}
		s := st.samples(units)
		points, err := join(s.Counts, dims, s.Times, nil)
		if err != nil {
			return nil, nil, err
		}
		sweeps, err := sweepsOf(points)
		return points, sweeps, err
	}
	summary := func(sweeps []sweep) string {
		var out []string
		for _, s := range sweeps {
			out = append(out, fmt.Sprintf("%v=%v@%d", s.end, s.count, s.firstSeenMs))
		}
		return strings.Join(out, " ")
	}

	// Repeated scrapes of one sweep collapse, first seen kept.
	var repeated [][4]float64
	for i := range 8 {
		at, count, end := 101_000+5_000*float64(i), 4.0, 100.0
		if at >= 121_000 {
			count, end = 6, 120
		}
		repeated = append(repeated, [4]float64{at, count, end, 1})
	}
	_, sweeps, err := joined(map[string][][4]float64{"a": repeated})
	eq(t, []any{summary(sweeps), err}, []any{"100=4@101000 120=6@121000", nil}, "repeated sweep times")

	// Two scrapes of one sweep disagreeing on its start are rejected.
	_, err = sweepsOf([]point{{key{"a", 100_000}, 5, 95, 85, 80, yes}, {key{"b", 110_000}, 5, 95, 82, 80, yes}})
	eq(t, isBadStore(err), true, "two starts for one sweep", err)
	// An impossible order within a sweep (start after end, or process after
	// start) is rejected.
	for _, p := range []point{
		{key{"a", 100_000}, 1, 90, 95, 80, yes}, // start 95 after end 90
		{key{"a", 100_000}, 1, 95, 90, 93, yes}, // process 93 after start 90
	} {
		_, err := sweepsOf([]point{p})
		eq(t, isBadStore(err), true, "out of order", p)
	}
	// A sweep may end after the scrape timestamp (the store stamps the scrape
	// before the probe publishes the sweep it reads), so it is not out of order.
	_, err = sweepsOf([]point{{key{"a", 95_000}, 1, 95.001, 90, 80, yes}}) // ends 1 ms after the 95 s scrape
	eq(t, err, nil, "a sweep ending after the scrape stamp is valid")

	// Replica b scrapes the same process 2 s later, and once at the same time.
	a := [][4]float64{{100_000, 3, 95, 1}, {105_000, 4, 104, 1}}
	points, sweeps, _ := joined(map[string][][4]float64{"a": a, "b": {{102_000, 3, 95, 1}, {105_000, 4, 104, 1}}})
	eq(t, []any{len(points), summary(sweeps)}, []any{4, "95=3@100000 104=4@105000"}, "replicas")
	_, _, err = joined(map[string][][4]float64{"a": a, "b": {{102_000, 99, 95, 1}}})
	eq(t, isBadStore(err), true, "two counts for one sweep", err)

	// Count series of one source, by state. 2^53 + 1 rounds to 2^53 in
	// float64, so the sum is checked in integers.
	times := []RawSeries{
		{map[string]string{"__name__": sweepEnd}, []Sample{{1_000, 1}}},
		{map[string]string{"__name__": sweepStart}, []Sample{{1_000, .5}}},
		{map[string]string{"__name__": processStart}, []Sample{{1_000, .1}}},
	}
	counted := func(values ...float64) ([]point, error) {
		var counts []RawSeries
		for i, v := range values {
			counts = append(counts, RawSeries{map[string]string{"__name__": units, "state": fmt.Sprint(i)}, []Sample{{1_000, v}}})
		}
		return join(counts, []string{"state"}, times, nil)
	}
	points, err = counted(maxCount-1, 1)
	eq(t, []any{points[0].count, err}, []any{maxCount, nil}, "2^53 is a count")
	for _, values := range [][]float64{{maxCount, 1}, {1e308, 1e308}, {maxCount + 2}, {-1}, {.5}, {math.NaN()}, {math.Inf(1)}} {
		_, err := counted(values...)
		eq(t, isBadStore(err) && !strings.Contains(err.Error(), "308"), true, values, err)
	}

	// Sweep and process times out of range.
	for _, c := range [][3]float64{{1, .5, .1}, {1e308, .5, .1}, {-1, .5, .1}, {1, 1e13, .1}, {1, .5, -5}, {math.NaN(), .5, .1}} {
		for i := range times {
			times[i].Samples[0].Value = c[i]
		}
		points, err := counted(1)
		_, errSweeps := sweepsOf(points)
		eq(t, []any{err, isBadStore(errSweeps)}, []any{nil, c != [3]float64{1, .5, .1}}, c)
	}
}

// Two samples of one source and time must agree, in any order (time series).
func TestConflictingTimeSamplesOfOneSource(t *testing.T) {
	sweepTimes := func(name string, values ...float64) []RawSeries {
		var out []RawSeries
		for _, v := range values {
			out = append(out, RawSeries{map[string]string{"__name__": name, "job": "j", "namespace": "ns"}, []Sample{{1_000, v}}})
		}
		return out
	}
	for _, ends := range [][]float64{{100, 101}, {101, 100}, {100, 100}} {
		points, err := join(nil, nil, slices.Concat(sweepTimes(sweepEnd, ends...), sweepTimes(sweepStart, 90), sweepTimes(processStart, 1)), nil)
		eq(t, []any{len(points), isUnavailable(err)}, []any{map[bool]int{true: 1}[ends[0] == ends[1]], ends[0] != ends[1]}, ends)
	}
}

// The allocatable capability: a sweep is allocatable only where its flag is 1.
// A missing or false flag, or another predicate, is unavailable, never zero.
func TestCapabilityIsTheAllocatableFlag(t *testing.T) {
	answers := func(t *testing.T, r probeapi.SummaryResponse, err error) {
		eq(t, []any{err, reached(r)}, []any{nil, true})
	}
	unavailable := func(t *testing.T, _ probeapi.SummaryResponse, err error) { eq(t, isUnavailable(err), true, err) }
	for _, c := range []struct {
		name  string
		build func(store)
		check func(t *testing.T, r probeapi.SummaryResponse, err error)
	}{
		{"allocatable is known", func(st store) {
			st.scrapeAlloc(-40, 0, -45, 1)
			st.scrapeAlloc(-20, 1, -25, 1)
			st.scrapeAlloc(0, 1, -5, 1)
		}, answers},
		{"a missing flag is unavailable, never a zero", func(st store) { st.scrapeAlloc(-40, 0, -45, math.NaN()); st.scrapeAlloc(-20, 1, -25, math.NaN()) }, unavailable},
		{"a flag of 0 is unavailable", func(st store) { st.scrapeAlloc(-40, 0, -45, 0); st.scrapeAlloc(-20, 1, -25, 1) }, unavailable},
		{"another predicate is no proof", func(st store) {
			st.scrapeAlloc(-40, 0, -45, math.NaN())
			st.add(known, ms(-40), 1, append([]string{"predicate", "updateEligible"}, selector...)...)
		}, unavailable},
	} {
		t.Run(c.name, func(t *testing.T) {
			st := store{}
			c.build(st)
			r, err := summarize(readyAlloc, params(-40, 0, 1), st.samples(hosts, known))
			c.check(t, r, err)
		})
	}
}

func TestPrepare(t *testing.T) {
	p := params(-600, 0, 1)
	with := func(edit func(*probeapi.SummaryParams)) probeapi.SummaryParams {
		c := p
		edit(&c)
		return c
	}
	for _, c := range []struct {
		name string
		f    probeapi.Filter
		p    probeapi.SummaryParams
		max  time.Duration
		want string // the field at fault, unsupported or ok
	}{
		{"a window of exactly the longest", ready, p, 600 * time.Second, "ok"},
		{"too long", ready, p, 599 * time.Second, "start"},
		{"empty", ready, params(0, 0, 1), day, "start"},
		{"end in the future", ready, params(-60, 10, 1), day, "end"},
		{"n past any count", ready, params(-600, 0, probeapi.MaxCount+1), day, "n"},
		{"the largest count", ready, params(-600, 0, probeapi.MaxCount), day, "ok"},
		{"negative write window", ready, with(func(p *probeapi.SummaryParams) { p.AddWindowSeconds = -1 }), day, "addWindowSeconds"},
		{"NaN write window", ready, with(func(p *probeapi.SummaryParams) { p.AddWindowSeconds = math.NaN() }), day, "addWindowSeconds"},
		{"write window past the longest", ready, with(func(p *probeapi.SummaryParams) { p.AddWindowSeconds = 86_401 }), day, "addWindowSeconds"},
		{"the longest write window", ready, with(func(p *probeapi.SummaryParams) { p.AddWindowSeconds = 86_400 }), day, "ok"},
		{"done is not a filter", probeapi.Filter{Type: probeapi.UnitTypeHost, Done: new(true)}, p, day, "unsupported"},
		{"a REST total is not a stage", probeapi.Filter{Type: probeapi.UnitTypeRestTotal, Resource: new(probeapi.RestResourceRack)}, p, day, "unsupported"},
		{"the REST source is not counted", probeapi.Filter{Type: probeapi.UnitTypeHost, Source: new(probeapi.SourceRest)}, p, day, "unsupported"},
	} {
		r, err := Prepare(context.Background(), testCtx(t0), c.f, c.p, c.max)
		got := fieldOf(err)
		switch {
		case err == nil && r.Start.Equal(c.p.Start.Time) && r.End.Equal(c.p.End.Time):
			got = "ok"
		case errors.As(err, new(Unsupported)):
			got = "unsupported"
		}
		eq(t, got, c.want, c.name, err)
	}
}

func fieldOf(err error) string {
	var e InvalidInput
	errors.As(err, &e)
	return e.Field
}

func TestFinalScrapeIsOneAtOrAfterTheEnd(t *testing.T) {
	ctx, c := context.Background(), testCtx(t0)
	st := store{}
	st.scrape(-25.3, 0, -45)
	st.scrape(-20.3, 1, -25)
	r, err := Prepare(ctx, c, ready, params(-60, -.3, 2), day)
	eq(t, err, nil)
	final, err := FinalScrapeSeen(ctx, c, r, st.samples(units))
	reply, errSummary := Summarize(ctx, c, r, st.samples(units))
	eq(t, []any{final, err, errSummary, reply.FinalScrapeSeen, opt(reply.FinalCount)}, []any{false, nil, nil, false, "1"}, "no tail yet")

	// The scrape exactly at the end counts: its count is the terminal one.
	st.scrape(-.3, 2, -5.3)
	final, err = FinalScrapeSeen(ctx, c, r, st.samples(units))
	reply, errSummary = Summarize(ctx, c, r, st.samples(units))
	eq(t, []any{final, err, errSummary, reply.FinalScrapeSeen, opt(reply.FinalCount)}, []any{true, nil, nil, true, "2"}, "the scrape at the end")

	// A count whose sweep times have not landed is not yet a scrape.
	late := store{}
	late.scrape(-40, 0, -45)
	late.add(units, ms(1), 2, append([]string{"type", "host", "stage", "ready", "state", "Ready"}, selector...)...)
	final, err = FinalScrapeSeen(ctx, c, r, late.samples(units))
	eq(t, []any{final, err}, []any{false, nil}, "a count without times")
}

func TestErrorsNeverCarrySampleValues(t *testing.T) {
	sentinel := 123_456_789.25
	st := store{}
	st.scrape(-20, sentinel, -25)
	_, err := summarize(ready, params(-60, 0, 1), st.samples(units))
	eq(t, err != nil && !strings.Contains(err.Error(), "123456789"), true, err)
	stray := RawSeries{map[string]string{"__name__": processStart, "instance": "http://u:token@p"}, []Sample{{1, -sentinel}}}
	_, err = summarize(ready, params(-60, 0, 1), Samples{Times: []RawSeries{stray}})
	eq(t, err == nil || !strings.Contains(err.Error(), "123456789") && !strings.Contains(err.Error(), "token"), true, err)
}

// One source has x="a,y=b", the other x="a" and y="b". Joined into one text
// they would read alike, and the first one's count would join the second's
// sweep.
func TestSourcesWhoseLabelsReadAlikeStayApart(t *testing.T) {
	for _, c := range []struct {
		times   []string
		crosses bool
	}{{[]string{"job", "j", "namespace", "ns", "x", "a,y=b"}, true}, {[]string{"job", "j", "namespace", "ns", "x", "a", "y", "b"}, false}} {
		st := store{}
		st.scrapeOf([]string{"job", "j", "namespace", "ns"}, ms(-40), 0, t0-45, testProcess)
		st.scrapeOf(c.times, ms(-20), 1, t0-25, testProcess)
		s := st.samples(units)
		s.Counts = []RawSeries{{map[string]string{"__name__": units, "type": "host", "stage": "ready", "state": "Ready", "job": "j", "namespace": "ns", "x": "a,y=b"}, []Sample{{ms(-20), 1}}}}
		r, err := summarize(ready, params(-40, 0, 1), s)
		eq(t, []any{err, reached(r)}, []any{nil, c.crosses}, c.times)
	}
}

func TestAtSecsNeverPanics(t *testing.T) {
	eq(t, AtSecs(1_761_213_605.5).UnixMilli(), int64(1_761_213_605_500))
	for _, past := range []float64{-1, math.Inf(-1), math.NaN()} {
		eq(t, AtSecs(past), jsonx.Time{Time: time.Unix(0, 0).UTC()}, past)
	}
	for _, future := range []float64{1e13, 1.8e19, 1e300, math.Inf(1)} {
		eq(t, AtSecs(future), maxTime, future)
	}
}

// TestProducerRun replays a producer-shaped host-ready run: a zero baseline,
// 18 hosts turning ready one per minute over twenty minutes, scraped every
// 5 s by two replicas, and a final scrape after the end.
func TestProducerRun(t *testing.T) {
	st := store{}
	readyAt := func(end float64) float64 { return min(math.Floor(max(end, 0)/60), 18) }
	for at := -30.0; at <= 1140; at += 5 {
		end := at - 5
		for _, replica := range []string{"a", "b"} {
			st.scrapeOf([]string{"instance", replica, "job", "j", "namespace", "ns"},
				ms(at+map[string]float64{"a": 0, "b": 2}[replica]), readyAt(end), t0+end, testProcess)
		}
	}
	// The window ends twenty minutes in; the clock stands after it.
	c := testCtx(t0 + 1200)
	req, err := Prepare(context.Background(), c, ready, params(-10, 1130, 18), day)
	eq(t, err, nil, "prepare")
	r, err := Summarize(context.Background(), c, req, st.samples(units))
	eq(t, []any{err, opt(r.FinalCount), r.FinalScrapeSeen}, []any{nil, "18", true}, "producer run")
	// Targets ceil(q*18) = 9, 17, 18... cross at 60*target s from t0, i.e.
	// 60*target + 10 s from the start at t0-10.
	eq(t, crossed(r), "9@550/9 17@1030/17 18@1090/18 18@1090/18 18@1090/18 18@1090/18")
	// Sweeps 5 s apart, each a 10 s sweep, no dispatch window: 5 + 10 + 0.
	eq(t, *r.TimingErrorSeconds, 15.0, crossed(r))
}

// The summary is bound to the run's process: a foreign process is not timed,
// an in-window restart is rejected, and older prehistory from another process
// is ignored before the run of the bound process.
func TestProcessBinding(t *testing.T) {
	const foreign = testProcess + 500
	run := func(st store, proc float64) {
		for _, x := range []struct{ at, count, end float64 }{{-40, 0, -45}, {-20, 1, -25}, {0, 1, -5}} {
			st.scrapeOf(selector, ms(x.at), x.count, t0+x.end, proc)
		}
	}
	for _, c := range []struct {
		name   string
		build  func(store)
		params probeapi.SummaryParams
		want   string // reached, unavailable or unknown
	}{
		{"the bound process answers", func(st store) { run(st, testProcess) }, params(-40, 0, 1), "reached"},
		{"foreign-only evidence is unavailable", func(st store) { run(st, foreign) }, params(-40, 0, 1), "unavailable"},
		{"a new process scraped in the window is a restart", func(st store) {
			st.scrapeOf(selector, ms(-40), 0, t0-45, testProcess)
			st.scrapeOf(selector, ms(-20), 1, t0-25, testProcess)
			st.scrapeOf(selector, ms(0), 2, t0-5, foreign)
		}, params(-40, 0, 1), "unavailable"},
		{"older prehistory from another process, scraped before the span, is ignored", func(st store) {
			st.scrapeOf(selector, ms(-300), 9, t0-305, foreign)
			st.scrapeOf(selector, ms(-250), 9, t0-255, foreign)
			run(st, testProcess)
		}, params(-40, 0, 1), "reached"},
		{"a foreign zero scraped before the span is not the baseline", func(st store) {
			st.scrapeOf(selector, ms(-50), 0, t0-55, foreign)
			st.scrapeOf(selector, ms(-20), 1, t0-25, testProcess)
			st.scrapeOf(selector, ms(0), 1, t0-5, testProcess)
		}, params(-40, 0, 1), "unknown"},
		{"a foreign sweep between the baseline and the window", func(st store) {
			st.scrapeOf(selector, ms(-43), 0, t0-45, testProcess)
			st.scrapeOf(selector, ms(-41), 5, t0-42, foreign)
			st.scrapeOf(selector, ms(-20), 1, t0-25, testProcess)
			st.scrapeOf(selector, ms(0), 1, t0-5, testProcess)
		}, params(-40, 0, 1), "unavailable"},
		// A foreign process scraped in the window with an old sweep is not prehistory.
		{"a foreign process scraped in the window with an old sweep", func(st store) {
			st.scrapeOf(selector, ms(-43), 0, t0-45, testProcess)
			st.scrapeOf(selector, ms(-20), 1, t0-25, testProcess)
			st.scrapeOf(selector, ms(-10), 3, t0-100, foreign)
		}, params(-40, 0, 1), "unavailable"},
		// A foreign sweep ending exactly at the baseline is not excluded.
		{"a foreign sweep ending exactly at the baseline", func(st store) {
			st.scrapeOf(selector, ms(-43), 0, t0-45, testProcess)
			st.scrapeOf(selector, ms(-42), 0, t0-45, foreign)
			st.scrapeOf(selector, ms(-20), 1, t0-25, testProcess)
			st.scrapeOf(selector, ms(0), 1, t0-5, testProcess)
		}, params(-40, 0, 1), "unavailable"},
		// A foreign sweep re-scraped in the span collapses keeping the earlier
		// first-seen time; every raw scrape is checked, so it is still a fault.
		{"a foreign sweep re-scraped in the window after a prehistory first scrape", func(st store) {
			st.scrapeOf(selector, ms(-43), 0, t0-45, testProcess)
			st.scrapeOf(selector, ms(-20), 1, t0-25, testProcess)
			st.scrapeOf(selector, ms(-90), 3, t0-100, foreign)
			st.scrapeOf(selector, ms(-10), 3, t0-100, foreign)
		}, params(-40, 0, 1), "unavailable"},
	} {
		t.Run(c.name, func(t *testing.T) {
			st := store{}
			c.build(st)
			r, err := summarize(ready, c.params, st.samples(units))
			switch c.want {
			case "unavailable":
				eq(t, isUnavailable(err), true, err)
			case "reached":
				eq(t, []any{err, reached(r)}, []any{nil, true}, err)
			case "unknown":
				eq(t, []any{err, reached(r)}, []any{nil, false}, err)
			}
		})
	}
}

// The final scrape is confirmed only by a scrape of the run's process at or
// after End. A scrape stamped at the millisecond before an End with a
// sub-millisecond fraction does not confirm it, across the whole accepted
// time range, including dates past the UnixNano limit of year 2262.
func TestFinalScrapeDoesNotRoundEndDown(t *testing.T) {
	for _, end := range []time.Time{
		time.Unix(1000, 500_000).UTC(),                    // a near-epoch End, 500 us fraction
		time.Date(2300, 1, 1, 0, 0, 0, 500_000, time.UTC), // year 2300, past the UnixNano limit
	} {
		r := &Request{End: end, in: input{process: testProcess}}
		endMs := end.UnixMilli() // the whole millisecond before End
		before := point{key: key{at: endMs}, process: testProcess}
		after := point{key: key{at: endMs + 1}, process: testProcess}
		foreign := point{key: key{at: endMs + 1}, process: testProcess + 1}
		eq(t, []any{r.finalScrape([]point{before}), r.finalScrape([]point{after}), r.finalScrape([]point{foreign})},
			[]any{false, true, false}, end)
	}
}

// pastDeadlineCtx is a context whose deadline has passed but whose Done
// notification has not fired: Err is still nil.
type pastDeadlineCtx struct {
	context.Context
	deadline time.Time
}

func (c pastDeadlineCtx) Deadline() (time.Time, bool) { return c.deadline, true }
func (pastDeadlineCtx) Err() error                    { return nil }
func (pastDeadlineCtx) Done() <-chan struct{}         { return nil }

func TestCheckCatchesAnElapsedDeadlineBeforeItsNotification(t *testing.T) {
	c := Context{Until: time.Now().Add(time.Hour)} // Until is in the future
	elapsed := pastDeadlineCtx{Context: context.Background(), deadline: time.Now().Add(-time.Second)}
	eq(t, c.Check(elapsed), error(Timeout{}), "elapsed deadline, delayed notification")
	cancelled, cancel := context.WithCancel(context.Background())
	cancel()
	eq(t, c.Check(cancelled), error(Cancelled{}), "an explicit cancel wins the tie")
}
