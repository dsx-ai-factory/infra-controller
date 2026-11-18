// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package history

import (
	"math"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// firmwarePoints checks every scrape, including repeated sweeps and an ABA
// between sweeps. Missing count/revision/attempt evidence is never a zero.
func firmwarePoints(r *Request, points []point, s Samples) error {
	if len(s.Caps) != 1 {
		return Unavailable{"firmware history evidence missing"}
	}
	fields := map[string]map[key]float64{}
	put := func(field string, row RawSeries, sample Sample) error {
		if fields[field] == nil {
			fields[field] = map[key]float64{}
		}
		k := key{sourceOf(row.Labels, "reader", "family", "kind", "result"), sample.AtMs}
		if old, ok := fields[field][k]; ok && old != sample.Value {
			return badStore("conflicting firmware evidence")
		}
		fields[field][k] = sample.Value
		return nil
	}
	for _, row := range s.Caps[0] {
		name := row.Labels["__name__"]
		field := ""
		switch {
		case name == probeapi.Prefix+"firmware_config_revision":
			field = "current"
		case name == probeapi.Prefix+"host_firmware_config_revision":
			field = "pinned"
		case name == probeapi.MetricSweepStart && row.Labels["family"] == "config_nico_api":
			field = "start"
		case name == probeapi.MetricSweepComplete && row.Labels["family"] == "config_nico_api":
			field = "end"
		case name == probeapi.Prefix+"last_attempt_failed" && row.Labels["kind"] == "none" && (row.Labels["family"] == "host" || row.Labels["family"] == "config_nico_api"):
			field = row.Labels["family"]
		}
		if field != "" {
			for _, sample := range row.Samples {
				if err := put(field, row, sample); err != nil {
					return err
				}
			}
		}
	}
	counts := map[key]bool{}
	for _, row := range s.Counts {
		for _, sample := range row.Samples {
			counts[key{sourceOf(row.Labels, probeapi.GaugeLabels(r.Spec.Metric)...), sample.AtMs}] = true
		}
	}
	last := map[string]point{}
	for _, p := range points {
		if r.in.base != nil && p.end < unixSecs(r.in.base.ReadAt.Time) {
			continue
		}
		if previous, ok := last[p.source]; ok && (p.count < previous.count || float64(p.at-previous.at) > 1000*(r.in.scrape+1)) {
			return Unavailable{"firmware counts reset or scrape evidence has a gap"}
		}
		last[p.source] = p
		if !counts[p.key] {
			return Unavailable{"firmware count sample missing"}
		}
		for _, field := range []string{"current", "pinned", "start", "end", "host", "config_nico_api"} {
			if _, ok := fields[field][p.key]; !ok {
				return Unavailable{"firmware source evidence missing"}
			}
		}
		start, end := fields["start"][p.key], fields["end"][p.key]
		at := float64(p.at) / 1000
		if fields["current"][p.key] != float64(r.in.firmwareRevision) || fields["pinned"][p.key] != float64(r.in.firmwareRevision) || fields["host"][p.key] != 1 || fields["config_nico_api"][p.key] != 1 || !(start >= p.process && start <= end && end <= at+1 && at-end <= 2*r.in.interval && end-start <= .75*r.in.interval) || math.IsNaN(start) || !(p.process <= p.start && p.start <= p.end && p.end <= at+1 && at-p.end <= 2*r.in.interval && p.end-p.start <= .75*r.in.interval) {
			return Unavailable{"firmware revision, source freshness or latest attempt changed"}
		}
	}
	return nil
}
