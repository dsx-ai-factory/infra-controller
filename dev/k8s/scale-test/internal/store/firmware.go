// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"math"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
)

const firmwarePassCounter = "carbide_machine_update_manager_completed_passes_total"
const firmwareCapGauge = "carbide_concurrent_machine_updates_available"

// FirmwareManager is one pod's real completed work and effective configured cap.
// The caller pins namespace/pod to its captured Kubernetes UID and restart count.
type FirmwareManager struct {
	Passes, Cap uint64
	From, At    time.Time
}

func (c *Client) FirmwareManagers(ctx context.Context, until time.Time, selector map[string]string, scrape time.Duration, since time.Time) (map[string]FirmwareManager, error) {
	if scrape < time.Second || scrape > time.Hour {
		return nil, unavailable("invalid manager scrape interval")
	}
	q := query{names: []string{firmwarePassCounter, firmwareCapGauge}, span: 3*scrape + time.Second}
	if len(selector) == 0 {
		return nil, unavailable("manager selector missing")
	}
	for k, v := range selector {
		if !poolLabel.MatchString(k) || k == "__name__" {
			return nil, unavailable("invalid manager selector")
		}
		q.matchers = append(q.matchers, [3]string{k, "=", v})
	}
	now := time.Now().Add(-c.delay)
	if !since.IsZero() {
		q.span = max(q.span, now.Sub(since)+scrape+time.Second)
	}
	rows, err := bounded(ctx, c.end(until), func(ctx context.Context) ([]history.RawSeries, error) { return c.raw(ctx, q, now) })
	if err != nil {
		return nil, err
	}
	return firmwareManagers(rows, now, scrape)
}

func firmwareManagers(rows []history.RawSeries, now time.Time, scrape time.Duration) (map[string]FirmwareManager, error) {
	result := map[string]FirmwareManager{}
	seen := map[string]map[string]bool{}
	for _, row := range rows {
		name := row.Labels["__name__"]
		if name != firmwarePassCounter && name != firmwareCapGauge {
			return nil, unavailable("unexpected manager metric")
		}
		ns, pod := row.Labels["namespace"], row.Labels["pod"]
		if ns == "" || pod == "" {
			return nil, unavailable("manager series lacks namespace/pod identity")
		}
		key := ns + "/" + pod
		if seen[key] == nil {
			seen[key] = map[string]bool{}
		}
		if seen[key][name] {
			return nil, unavailable("ambiguous manager series")
		}
		seen[key][name] = true
		v := row.Samples
		if len(v) < 2 || v[len(v)-1].AtMs < now.Add(-scrape-time.Second).UnixMilli() {
			return nil, unavailable("manager samples missing or stale")
		}
		for i, s := range v {
			if !(s.Value >= 0 && s.Value <= 1<<53 && math.Trunc(s.Value) == s.Value) || s.AtMs > now.UnixMilli() || i > 0 && (s.AtMs <= v[i-1].AtMs || s.AtMs-v[i-1].AtMs > (scrape+time.Second).Milliseconds() || name == firmwarePassCounter && s.Value < v[i-1].Value) {
				return nil, unavailable("invalid, reset or gapped manager samples")
			}
		}
		value := result[key]
		if first := time.UnixMilli(v[0].AtMs); value.From.IsZero() || first.After(value.From) {
			value.From = first
		}
		last := v[len(v)-1]
		if name == firmwarePassCounter {
			value.Passes = uint64(last.Value)
			value.At = time.UnixMilli(last.AtMs)
		} else {
			value.Cap = uint64(last.Value)
		}
		result[key] = value
	}
	if len(result) == 0 {
		return nil, unavailable("no real completed manager pass counter")
	}
	for k := range result {
		if len(seen[k]) != 2 {
			return nil, unavailable("manager cap or completed-pass counter missing")
		}
	}
	return result, nil
}
