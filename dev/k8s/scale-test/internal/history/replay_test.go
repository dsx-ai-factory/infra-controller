// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package history_test replays a capture of the site-health-probe collector
// through the fleet-time math and checks it against the captured raw samples.
// The synthetic fixture runs as a unit test; the acceptance test replays a
// real capture. See ACCEPTANCE.md.
package history_test

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// MaxCaptureBytes bounds a mounted capture read: a ConfigMap is under 1 MiB,
// but a mounted PVC file is not, so an oversize one is refused before decoding
// rather than allocated whole in the test pod.
const MaxCaptureBytes = 8 << 20

// loadCapture reads path, refusing a file over max bytes before decoding.
func loadCapture(path string, max int64) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()
	data, err := io.ReadAll(io.LimitReader(f, max+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > max {
		return nil, fmt.Errorf("capture is larger than %d bytes", max)
	}
	return data, nil
}

func TestLoadCaptureRefusesOversize(t *testing.T) {
	path := filepath.Join(t.TempDir(), "c.json")
	if err := os.WriteFile(path, []byte("12345678901"), 0o600); err != nil { // 11 bytes
		t.Fatal(err)
	}
	if _, err := loadCapture(path, 10); err == nil {
		t.Fatal("loadCapture accepted a file over the cap")
	}
	if _, err := loadCapture(path, 11); err != nil { // at the cap
		t.Fatalf("loadCapture refused a file at the cap: %v", err)
	}
}

type captureSeries struct {
	Labels  map[string]string `json:"labels"`
	Samples [][2]float64      `json:"samples"`
}

type captureCrossing struct {
	Q             float64 `json:"q"`
	Target        uint64  `json:"target"`
	ReachedAtUnix float64 `json:"reachedAtUnix"`
	Seconds       float64 `json:"seconds"`
}

type capture struct {
	Metadata probeapi.MetricsInfo `json:"metadata"`
	NowUnix  float64              `json:"nowUnix"`
	Start    string               `json:"start"`
	End      string               `json:"end"`
	N        uint64               `json:"n"`
	Counts   []captureSeries      `json:"counts"`
	Times    []captureSeries      `json:"times"`
	Caps     [][]captureSeries    `json:"caps"`
	Expected struct {
		Crossings             []captureCrossing `json:"crossings"`
		FinalCount            uint64            `json:"finalCount"`
		FinalSweepAtUnix      float64           `json:"finalSweepAtUnix"`
		MinTimingErrorSeconds float64           `json:"minTimingErrorSeconds"`
	} `json:"expected"`
}

func raw(in []captureSeries) []history.RawSeries {
	var out []history.RawSeries
	for _, s := range in {
		r := history.RawSeries{Labels: s.Labels}
		for _, x := range s.Samples {
			r.Samples = append(r.Samples, history.Sample{AtMs: int64(x[0]), Value: x[1]})
		}
		out = append(out, r)
	}
	return out
}

func capsOf(in [][]captureSeries) [][]history.RawSeries {
	var out [][]history.RawSeries
	for _, c := range in {
		out = append(out, raw(c))
	}
	return out
}

// replayCapture checks every crossing, the final count, the process binding
// and the timing bound against the capture's independently-derived expected.
func replayCapture(t *testing.T, data []byte) {
	t.Helper()
	var c capture
	if err := json.Unmarshal(data, &c); err != nil {
		t.Fatal(err)
	}
	meta, err := history.NewProbeMetadata(c.Metadata)
	if err != nil {
		t.Fatalf("metadata: %v", err)
	}
	start, err1 := time.Parse(time.RFC3339, c.Start)
	end, err2 := time.Parse(time.RFC3339, c.End)
	if err1 != nil || err2 != nil {
		t.Fatalf("start/end: %v %v", err1, err2)
	}
	ctx := context.Background()
	// Now is the capture's logical clock; Until is the real deadline.
	hc := history.Context{Probe: meta, Now: func() time.Time { return time.Unix(int64(c.NowUnix), 0).UTC() },
		Until: time.Now().Add(time.Hour)}
	ready := probeapi.Filter{Type: probeapi.UnitTypeHost, Stage: new(probeapi.StageReady)}
	req, err := history.Prepare(ctx, hc, ready, probeapi.SummaryParams{
		Start: jsonx.Time{Time: start}, End: jsonx.Time{Time: end}, N: c.N,
	}, 24*time.Hour)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	// A clean result confirms the source binding: a foreign process would fail.
	r, err := history.Summarize(ctx, hc, req, history.Samples{Counts: raw(c.Counts), Times: raw(c.Times), Caps: capsOf(c.Caps)})
	if err != nil {
		t.Fatalf("summarize: %v", err)
	}
	if len(r.Crossings) != len(c.Expected.Crossings) {
		t.Fatalf("%d crossings, want %d", len(r.Crossings), len(c.Expected.Crossings))
	}
	for i, want := range c.Expected.Crossings {
		got := r.Crossings[i]
		if got.Q != want.Q || got.Target != want.Target {
			t.Errorf("crossing %d: got q=%v target=%d, want q=%v target=%d", i, got.Q, got.Target, want.Q, want.Target)
		}
		if got.ReachedAt == nil || !got.ReachedAt.Equal(history.AtSecs(want.ReachedAtUnix).Time) || got.Seconds == nil || *got.Seconds != want.Seconds {
			t.Errorf("crossing %d: got %v/%v, want %v s at %v", i, got.ReachedAt, got.Seconds, want.Seconds, want.ReachedAtUnix)
		}
	}
	if r.FinalCount == nil || *r.FinalCount != c.Expected.FinalCount {
		t.Errorf("final count %v, want %d", r.FinalCount, c.Expected.FinalCount)
	}
	if r.FinalSweepAt == nil || !r.FinalSweepAt.Equal(history.AtSecs(c.Expected.FinalSweepAtUnix).Time) {
		t.Errorf("final sweep at %v, want %v", r.FinalSweepAt, c.Expected.FinalSweepAtUnix)
	}
	// The reported bound must not underestimate the captured uncertainty.
	if r.TimingErrorSeconds == nil || *r.TimingErrorSeconds < c.Expected.MinTimingErrorSeconds {
		t.Errorf("timing error %v underestimates the captured bound %v", r.TimingErrorSeconds, c.Expected.MinTimingErrorSeconds)
	}
	if !r.FinalScrapeSeen {
		t.Error("the capture must include the final scrape after the window end")
	}
}

// TestReplaySyntheticCapture exercises the replay path on the committed
// synthetic fixture in ordinary unit tests. It is not real evidence.
func TestReplaySyntheticCapture(t *testing.T) {
	data, err := loadCapture("testdata/synthetic-capture.json", MaxCaptureBytes)
	if err != nil {
		t.Fatal(err)
	}
	replayCapture(t, data)
}
