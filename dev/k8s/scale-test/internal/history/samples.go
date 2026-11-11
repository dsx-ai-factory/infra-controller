// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package history

import (
	"cmp"
	"maps"
	"math"
	"slices"
	"strings"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// Stored samples to sweeps. Every scrape carries a count and the end time E
// of the sweep it came from (one snapshot). Samples join per scrape on
// (source, timestamp), and scrapes collapse to sweeps by E. v1 reads one
// probe process: a second process start is a restart or a replaced pod, and
// the run is rejected rather than folded.

const (
	sweepEnd     = probeapi.MetricSweepComplete
	sweepStart   = probeapi.MetricSweepStart
	processStart = probeapi.MetricProcessStart
	// lastTime is the last Unix time a probe sample can have or show: the
	// end of year 9999.
	lastTime = 253_402_300_799.0
)

// RawSeries is one series and its samples, as the store returned them.
type RawSeries struct {
	Labels  map[string]string
	Samples []Sample
}

// Sample is one stored value at a Unix time in milliseconds.
type Sample struct {
	AtMs  int64
	Value float64
}

// IsTime reports whether secs is a Unix time a probe sample can have or
// show: from the epoch to the end of year 9999.
func IsTime(secs float64) bool { return secs >= 0 && secs <= lastTime }

// isCount reports whether v is a number of units: whole, from 0 to MaxCount.
func isCount(v float64) bool {
	return v >= 0 && v <= float64(probeapi.MaxCount) && v == math.Trunc(v)
}

// sourceOf is the scrape source of a series: its label pairs in label order,
// without the name and dims. Each label and value ends in two NULs and
// doubles each NUL of its own as NUL 1, so two label sets never share a key
// (x="a,y=b" is not x="a",y="b") and keys sort as the pairs do.
func sourceOf(labels map[string]string, dims ...string) string {
	var b strings.Builder
	for _, k := range slices.Sorted(maps.Keys(labels)) {
		if k != "__name__" && !slices.Contains(dims, k) {
			for _, s := range [2]string{k, labels[k]} {
				b.WriteString(strings.ReplaceAll(s, "\x00", "\x00\x01") + "\x00\x00")
			}
		}
	}
	return b.String()
}

type key struct {
	source string
	at     int64
}

// flag is what a sweep's scrapes show about a capability. The weakest stands:
// any no stays no, and missing evidence is never yes.
type flag int8

const (
	no flag = iota
	unknown
	yes
)

// point is one scrape joined across series: its count and the sweep it
// shows, in Unix seconds.
type point struct {
	key
	count, end, start, process float64
	capable                    flag
}

// capSeries is the flag series of one capability: only the series of the
// gauge name with label equal to value are its own. A flag is met only where
// every sample is exactly 1.
type capSeries struct {
	series             []RawSeries
	name, label, value string
}

// putTimes records the samples of s by source and time. Two samples of one
// source and time must agree, or the result would depend on the order they
// arrive in.
func putTimes(m map[key]float64, s RawSeries) error {
	source := sourceOf(s.Labels, probeapi.TimeDims[:]...)
	for _, x := range s.Samples {
		k := key{source, x.AtMs}
		if v, ok := m[k]; ok && v != x.Value {
			return badStore("two samples of one source and time differ")
		}
		m[k] = x.Value
	}
	return nil
}

// join joins count, time and capability samples on (source, timestamp),
// sorted by time, then source. A point needs a sweep end, a sweep start and
// a process start. A missing count is 0, as a store client reads counts
// after times. Counts add up in integers: every sample, and every sum, is a
// whole number from 0 to MaxCount, or the store is not showing the probe's
// series.
func join(counts []RawSeries, countDims []string, times []RawSeries, caps []capSeries) ([]point, error) {
	count := map[key]uint64{}
	for _, s := range counts {
		source := sourceOf(s.Labels, countDims...)
		for _, x := range s.Samples {
			k := key{source, x.AtMs}
			if !isCount(x.Value) || count[k]+uint64(x.Value) > probeapi.MaxCount {
				return nil, badStore("a count is not a number of units")
			}
			count[k] += uint64(x.Value)
		}
	}
	byName := map[string]map[key]float64{sweepEnd: {}, sweepStart: {}, processStart: {}}
	for _, s := range times {
		if m := byName[s.Labels["__name__"]]; m != nil {
			if err := putTimes(m, s); err != nil {
				return nil, err
			}
		}
	}
	flags := make([]map[key]bool, len(caps))
	for i, c := range caps {
		flags[i] = map[key]bool{}
		for _, s := range c.series {
			if v, ok := s.Labels[c.label]; s.Labels["__name__"] != c.name || !ok || v != c.value {
				continue
			}
			source := sourceOf(s.Labels, c.label)
			for _, x := range s.Samples {
				met, seen := flags[i][key{source, x.AtMs}]
				flags[i][key{source, x.AtMs}] = (met || !seen) && x.Value == 1
			}
		}
	}
	var points []point
	for k, end := range byName[sweepEnd] {
		start, okStart := byName[sweepStart][k]
		process, okProcess := byName[processStart][k]
		if !okStart || !okProcess {
			continue
		}
		p := point{k, float64(count[k]), end, start, process, yes}
		for _, f := range flags {
			met, seen := f[k]
			if !seen {
				p.capable = unknown
				break
			}
			if !met {
				p.capable = no
			}
		}
		points = append(points, p)
	}
	slices.SortFunc(points, func(a, b point) int {
		return cmp.Or(cmp.Compare(a.at, b.at), strings.Compare(a.source, b.source))
	})
	return points, nil
}

// LatestSweep is the newest scrape in times, joined on (source, timestamp) as
// a summary joins them, that shows a sweep of the probe process started at
// process, by exact bits: its time and the sweep's end. ok is false when no
// scrape shows one, so an earlier process's sweep is never its evidence.
func LatestSweep(times []RawSeries, process float64) (atMs int64, end float64, ok bool, err error) {
	points, err := join(nil, nil, times, nil)
	for _, p := range slices.Backward(points) {
		if math.Float64bits(p.process) == math.Float64bits(process) {
			return p.at, p.end, true, err
		}
	}
	return 0, 0, false, err
}

// sweep is one sweep, as its first scrape saw it. process is the probe
// process that produced it; the summary binds the window to one process and
// rejects a foreign one.
type sweep struct {
	process, start, end, count float64
	firstSeenMs                int64
	capable                    flag
}

// sweepsOf collapses scrapes (sorted by time) to sweeps: one per
// (process, end), sorted by end. Repeated scrapes of one sweep, from one
// scraper or several, collapse, and every scrape's capability counts. Scrapes
// of one sweep must agree on its count and start, or the store mixes probes.
// Each sweep's own times must be ordered: process <= start <= end. The scrape
// timestamp is not compared with the end, since the store stamps a scrape
// before serving it and the probe can publish a sweep during collection. Two
// processes are kept apart here; the summary binds the window to the run's.
func sweepsOf(points []point) ([]sweep, error) {
	seen := map[[2]uint64]int{}
	var out []sweep
	for _, p := range points {
		// Each sample is finite, but one can be out of range.
		if !(p.count >= 0 && p.count <= float64(probeapi.MaxCount)) {
			return nil, badStore("a count is not a number of units")
		}
		if !IsTime(p.end) || !IsTime(p.start) || !IsTime(p.process) {
			return nil, badStore("a sweep or process time is out of range")
		}
		if !(p.process <= p.start && p.start <= p.end) {
			return nil, badStore("a sweep's process, start and end times are out of order")
		}
		k := [2]uint64{math.Float64bits(p.process), math.Float64bits(p.end)}
		j, ok := seen[k]
		switch {
		case !ok:
			seen[k] = len(out)
			out = append(out, sweep{p.process, p.start, p.end, p.count, p.at, p.capable})
		case out[j].count != p.count:
			return nil, badStore("inconsistent sources: one sweep has two counts")
		case math.Float64bits(out[j].start) != math.Float64bits(p.start):
			return nil, badStore("inconsistent sources: one sweep has two start times")
		default:
			out[j].capable = min(out[j].capable, p.capable)
		}
	}
	slices.SortFunc(out, func(a, b sweep) int {
		return cmp.Or(cmp.Compare(a.end, b.end), cmp.Compare(math.Float64bits(a.process), math.Float64bits(b.process)))
	})
	return out, nil
}
