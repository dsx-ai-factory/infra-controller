// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"encoding/json"
	"math"
	"regexp"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
)

var poolLabel = regexp.MustCompile(`^[a-zA-Z_][a-zA-Z0-9_]*$`)

// PoolHealthy checks each API replica separately over the preceding minute.
// Missing, stale or gapped samples fail. Returned identities detect lost replicas.
func (c *Client) PoolHealthy(ctx context.Context, until time.Time, selector map[string]string) ([]string, error) {
	if len(selector) == 0 {
		return nil, unavailable("API pool selector is missing")
	}
	q := query{names: []string{"carbide_db_pool_idle_conns"}, span: 93 * time.Second}
	for k, v := range selector {
		if !poolLabel.MatchString(k) || k == "__name__" {
			return nil, unavailable("invalid pool selector")
		}
		q.matchers = append(q.matchers, [3]string{k, "=", v})
	}
	now := time.Now()
	rows, err := bounded(ctx, c.end(until), func(ctx context.Context) ([]history.RawSeries, error) { return c.raw(ctx, q, now) })
	if err != nil {
		return nil, err
	}
	return poolHealth(rows, now)
}

func poolHealth(rows []history.RawSeries, now time.Time) ([]string, error) {
	if len(rows) == 0 {
		return nil, unavailable("no API pool samples")
	}
	ids := []string{}
	for _, row := range rows {
		samples := row.Samples
		if len(samples) < 3 || samples[0].AtMs > now.Add(-time.Minute).UnixMilli() || samples[len(samples)-1].AtMs < now.Add(-31*time.Second).UnixMilli() {
			return nil, unavailable("incomplete API pool window")
		}
		begin := 0
		for i, s := range samples {
			if s.AtMs <= now.Add(-time.Minute).UnixMilli() {
				begin = i
			}
		}
		positive := false
		for i, s := range samples {
			if s.AtMs < 0 || !(s.Value >= 0 && s.Value <= float64(1<<53) && s.Value == math.Trunc(s.Value)) || s.AtMs > now.UnixMilli() || i > 0 && (s.AtMs <= samples[i-1].AtMs || s.AtMs-samples[i-1].AtMs > 31000) {
				return nil, unavailable("invalid API pool samples")
			}
			positive = positive || i >= begin && s.Value > 0
		}
		if !positive {
			return nil, unavailable("an API pool had zero idle connections for 60 seconds")
		}
		raw, _ := json.Marshal(row.Labels)
		id := string(raw)
		if slices.Contains(ids, id) {
			return nil, unavailable("duplicate API pool series")
		}
		ids = append(ids, id)
	}
	slices.Sort(ids)
	return ids, nil
}

// ProbeDemand measures all shared-gate calls, including retries and latency
// probes. A counter reset, missing API or incomplete interval refuses calibration.
func (c *Client) ProbeDemand(ctx context.Context, until time.Time, span time.Duration, extraREST ...float64) error {
	now := time.Now()
	q := query{names: []string{"carbide_site_health_probe_calls_total"}, matchers: c.selector(), span: span + 33*time.Second}
	rows, err := bounded(ctx, c.end(until), func(ctx context.Context) ([]history.RawSeries, error) { return c.raw(ctx, q, now) })
	if err != nil {
		return err
	}
	totals := map[string]float64{}
	for _, row := range rows {
		api := row.Labels["api"]
		if api != "nico-api" && api != "nico-rest-api" {
			return unavailable("unknown probe API counter")
		}
		v := row.Samples
		if len(v) < 2 {
			return unavailable("missing probe call samples")
		}
		first, last := v[0], v[len(v)-1]
		if first.AtMs > now.Add(-span).UnixMilli() || last.AtMs < now.Add(-31*time.Second).UnixMilli() {
			return unavailable("incomplete probe call interval")
		}
		for i, s := range v {
			if s.AtMs < 0 || !(s.Value >= 0 && s.Value <= float64(1<<53) && s.Value == math.Trunc(s.Value)) || i > 0 && (s.Value < v[i-1].Value || s.AtMs <= v[i-1].AtMs || s.AtMs-v[i-1].AtMs > 31000) {
				return unavailable("probe call counter reset or gap")
			}
		}
		totals[api] += (last.Value - first.Value) / (float64(last.AtMs-first.AtMs) / 1000)
	}
	if len(extraREST) > 1 {
		return unavailable("invalid sidecar call accounting")
	}
	if len(extraREST) == 1 {
		if !(extraREST[0] >= 0 && extraREST[0] < 1) {
			return unavailable("sidecar call accounting missing or saturated")
		}
	}
	for _, api := range []string{"nico-api", "nico-rest-api"} {
		n, ok := totals[api]
		if api == "nico-rest-api" && len(extraREST) == 1 {
			n += extraREST[0]
		}
		if !ok || !(n < 1) {
			return unavailable("probe source demand is missing or at least one call per second")
		}
	}
	return nil
}
