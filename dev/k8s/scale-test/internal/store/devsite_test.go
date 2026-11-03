// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build devsite

package store_test

import (
	"context"
	"errors"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/store"
)

var readyFilter = probeapi.Filter{Type: probeapi.UnitTypeHost, Stage: new(probeapi.StageReady)}

// realStore builds a store client pinned to the live probe's metadata from the
// store at SCALE_TEST_STORE_URL that scrapes the probe at SCALE_TEST_PROBE_URL,
// failing rather than skipping when either is unset. Optional _TOKEN_FILE,
// _CA_FILE and _INGESTION_DELAY cover auth, a private CA or remote write.
func realStore(t *testing.T) (*store.Client, store.Config, history.ProbeMetadata, context.Context) {
	storeURL, probeURL := os.Getenv("SCALE_TEST_STORE_URL"), os.Getenv("SCALE_TEST_PROBE_URL")
	if storeURL == "" || probeURL == "" {
		t.Fatal("SCALE_TEST_STORE_URL and SCALE_TEST_PROBE_URL must be set to run the dev-site store test")
	}
	ctx := context.Background()
	p, err := probe.NewHTTP(probeURL, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	cfg, err := p.Config(ctx, probe.ConfigFilter{})
	if err != nil {
		t.Fatalf("Config: %v", err)
	}
	if cfg.Metrics == nil {
		t.Fatal("the probe declares no metrics metadata; set it up before this test")
	}
	meta, err := history.NewProbeMetadata(*cfg.Metrics)
	if err != nil {
		t.Fatalf("probe metadata: %v", err)
	}
	sc := store.Config{URL: storeURL}
	if f := os.Getenv("SCALE_TEST_STORE_TOKEN_FILE"); f != "" {
		sc.BearerTokenFile = &f
	}
	if f := os.Getenv("SCALE_TEST_STORE_CA_FILE"); f != "" {
		sc.CAFile = &f
	}
	if d := os.Getenv("SCALE_TEST_STORE_INGESTION_DELAY"); d != "" {
		delay, err := strconv.ParseUint(d, 10, 64)
		if err != nil {
			t.Fatalf("SCALE_TEST_STORE_INGESTION_DELAY: %v", err)
		}
		sc.IngestionDelaySeconds = delay
	}
	client, err := store.New(sc, meta, store.DefaultOptions)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	return client, sc, meta, ctx
}

// TestAgainstARealStore exercises the read path against a real store: query,
// matrix decode or contract drift fails. A standing fleet's crossings must be
// unknown, not fabricated; a selector no series carries fails preflight; a
// cancelled summary is typed Cancelled. A real transition is the next test.
func TestAgainstARealStore(t *testing.T) {
	client, sc, meta, ctx := realStore(t)

	if err := client.Preflight(ctx, time.Now().Add(30*time.Second), probeapi.FamilyHost); err != nil {
		t.Fatalf("Preflight: %v", err)
	}
	status, err := client.StoreStatus(ctx, time.Now().Add(30*time.Second))
	if err != nil {
		t.Fatalf("StoreStatus: %v", err)
	}
	if status.LastSampleAt == nil || status.SweepCompleteAt == nil {
		t.Fatal("the store has no host sweep end sample")
	}

	end := time.Now().Add(-time.Minute) // a minute ago, so the final scrape has landed
	params := probeapi.SummaryParams{Start: jsonx.Time{Time: end.Add(-10 * time.Minute).UTC()}, End: jsonx.Time{Time: end.UTC()}, N: 1}
	r, err := client.Summarize(ctx, time.Now().Add(2*time.Minute), readyFilter, params)
	if err != nil {
		t.Fatalf("Summarize: %v", err)
	}
	if r.Family != probeapi.FamilyHost || len(r.Crossings) != len(probeapi.FixedQuantiles) || !r.FinalScrapeSeen {
		t.Fatalf("summary did not read the window: %+v", r)
	}
	for _, c := range r.Crossings {
		if c.ReachedAt != nil {
			t.Fatalf("a standing fleet has no fresh zero baseline, so crossings must be unknown: %+v", r.Crossings)
		}
	}

	wrong := meta
	wrong.Info.Selector = map[string]string{"job": "no-such-job"}
	if wc, nerr := store.New(sc, wrong, store.DefaultOptions); nerr != nil {
		t.Fatalf("New wrong selector: %v", nerr)
	} else if perr := wc.Preflight(ctx, time.Now().Add(30*time.Second), probeapi.FamilyHost); perr == nil {
		t.Fatal("a wrong selector must fail preflight")
	}

	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	started := time.Now()
	_, cerr := client.Summarize(cancelled, time.Now().Add(2*time.Minute), readyFilter, params)
	if !errors.As(cerr, new(history.Cancelled)) {
		t.Fatalf("a cancelled summary must be Cancelled, got %v", cerr)
	}
	if elapsed := time.Since(started); elapsed > 5*time.Second {
		t.Fatalf("a cancelled summary must return promptly, took %v", elapsed)
	}
}

// TestControlledTransition times a disposable fresh zero-to-ready cohort over
// the window the operator drove, failing when any capture input is unset so the
// positive suite cannot pass without a real transition. The SCALE_TEST_CAPTURE_*
// window, size and independent per-quantile answers come from the environment.
func TestControlledTransition(t *testing.T) {
	client, _, _, ctx := realStore(t)

	targets := strings.Split(os.Getenv("SCALE_TEST_CAPTURE_TARGETS"), ",")
	reached := strings.Split(os.Getenv("SCALE_TEST_CAPTURE_REACHED"), ",")
	n, nerr := strconv.ParseUint(os.Getenv("SCALE_TEST_CAPTURE_N"), 10, 64)
	start, serr := time.Parse(time.RFC3339, os.Getenv("SCALE_TEST_CAPTURE_START"))
	fin, ferr := time.Parse(time.RFC3339, os.Getenv("SCALE_TEST_CAPTURE_END"))
	minError, merr := strconv.ParseFloat(os.Getenv("SCALE_TEST_CAPTURE_MIN_TIMING_ERROR"), 64)
	if nerr != nil || serr != nil || ferr != nil || merr != nil {
		t.Fatal("SCALE_TEST_CAPTURE_START, _END, _N, _TARGETS, _REACHED and _MIN_TIMING_ERROR must be set")
	}

	params := probeapi.SummaryParams{Start: jsonx.Time{Time: start.UTC()}, End: jsonx.Time{Time: fin.UTC()}, N: n}
	r, err := client.Summarize(ctx, time.Now().Add(2*time.Minute), readyFilter, params)
	if err != nil {
		t.Fatalf("Summarize: %v", err)
	}
	if r.Family != probeapi.FamilyHost || !r.FinalScrapeSeen {
		t.Fatalf("controlled window not read: %+v", r)
	}
	// The conservative bound is sweep gap plus sweep duration plus dispatch window,
	// reported as TimingErrorSeconds; the raw-curve times match to timestamp precision.
	if err := checkTimingError(r.TimingErrorSeconds, minError); err != nil {
		t.Fatal(err)
	}
	if err := crossingsMatch(r.Crossings, targets, reached, time.Second); err != nil {
		t.Fatal(err)
	}
}
