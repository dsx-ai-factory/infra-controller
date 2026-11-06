// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

import (
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

func at(sec int) jsonx.Time {
	base, _ := time.Parse(time.RFC3339, "2026-10-23T10:00:00Z")
	return jsonx.Time{Time: base.Add(time.Duration(sec) * time.Second)}
}

// sample is a complete failed run with one stage and one invariant.
func sample() *RunResult {
	return &RunResult{
		SchemaVersion: SchemaVersion, Test: "burst", RunID: "r1", ExecutionID: "r1-abcde", DryRun: ptr(false), Outcome: OutcomeOf(Failed),
		Reasons:    []string{"stage host.endToEnd: 50 units failed"},
		Clock:      RunClock{StartedAt: at(-300), Deadline: ptr(at(2400)), EndedAt: at(2700), ClockSyncAssumption: ClockSyncAssumption},
		Lock:       LockResult{Name: "nico-scale-test-site-lock", Kind: "test", AcquiredAt: ptr(at(-300)), FinalState: ptr("final")},
		Stages:     []StageResult{stage(StageFailed, 4500, 4450, 50)},
		Invariants: []InvariantResult{inv("links", true, 40)},
		Journal:    JournalSummary{Ops: ptr(uint64(10)), Attempts: ptr(uint64(12)), Unclear: ptr(uint64(0))},
		Result:     ResultRef{Claim: "r1-journal", Path: "/journal/r1.result.json"},
	}
}

// TestResultFileReplacedWholeAndPreservedOnFailure proves result.json is one whole file replaced in place by the next
// write with no temp left behind, and that a failed write leaves the prior whole file intact, never partial.
func TestResultFileReplacedWholeAndPreservedOnFailure(t *testing.T) {
	dir := t.TempDir()
	f := ResultFor(dir, "r1")
	if err := f.Write(sample()); err != nil {
		t.Fatal(err)
	}
	first, err := os.ReadFile(f.Path)
	if err != nil || !json.Valid(first) {
		t.Fatalf("first result not whole JSON: %v", err)
	}
	// A second write replaces the file: a reader sees the new whole result, not a partial one.
	s2 := sample()
	s2.Outcome = OutcomeOf(Passed)
	if err := f.Write(s2); err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(f.Path)
	if err != nil || !json.Valid(raw) {
		t.Fatalf("second result not whole JSON: %v", err)
	}
	var got map[string]json.RawMessage
	_ = json.Unmarshal(raw, &got)
	if string(got["verdict"]) != `"passed"` {
		t.Errorf("the file was not replaced: verdict %s", got["verdict"])
	}
	if _, err := os.Stat(f.Path + ".tmp"); !os.IsNotExist(err) {
		t.Error("a temp file was left behind")
	}
	// A failed write (a non-finite number) never touches the file: the prior whole result is preserved, not truncated.
	bad := sample()
	bad.Stages[0].Units.FailedPct = ptr(math.NaN())
	if err := f.Write(bad); err == nil {
		t.Fatal("Write accepted a non-finite number")
	}
	after, err := os.ReadFile(f.Path)
	if err != nil || string(after) != string(raw) {
		t.Errorf("a failed write changed the result file: err=%v", err)
	}
}

// TestNonFiniteNumberIsNeverWritten proves a result with a non-finite number is an error and leaves no result file.
func TestNonFiniteNumberIsNeverWritten(t *testing.T) {
	for i, nan := range []float64{math.NaN(), math.Inf(1), math.Inf(-1)} {
		full := sample()
		full.Stages[0].Units.FailedPct = ptr(nan)
		if _, err := jsonx.Encode(full); err == nil {
			t.Errorf("%d: a non-finite number serialized", i)
		}
		dir := t.TempDir()
		f := ResultFor(dir, "r1")
		if err := f.Write(full); err == nil {
			t.Errorf("%d: Write accepted a non-finite number", i)
		}
		if _, err := os.Stat(f.Path); !os.IsNotExist(err) {
			t.Errorf("%d: the result file was written anyway", i)
		}
	}
}

func TestRunResultFlattensOutcomeAndRoundTrips(t *testing.T) {
	raw, err := jsonx.Encode(sample())
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]json.RawMessage
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatal(err)
	}
	if string(got["verdict"]) != `"failed"` || string(got["exitCode"]) != "1" {
		t.Errorf("outcome did not flatten: verdict=%s exitCode=%s", got["verdict"], got["exitCode"])
	}
	// A nil-pointer field writes null; the reasons list is never null.
	if string(got["reasons"]) == "null" {
		t.Error("reasons must be a list, never null")
	}
}

func TestJournalSummaryUnknownVersusNoneWritten(t *testing.T) {
	var unknown JournalSummary
	if unknown.Ops != nil || unknown.Attempts != nil || unknown.Unclear != nil {
		t.Error("the zero JournalSummary must be all unknown (nil)")
	}
	none := NoneWritten()
	if *none.Ops != 0 || *none.Attempts != 0 || *none.Unclear != 0 {
		t.Error("NoneWritten must be all 0")
	}
}

func TestResultForPath(t *testing.T) {
	if got := ResultFor("/journal", "r1").Path; got != filepath.FromSlash("/journal/r1.result.json") {
		t.Errorf("ResultFor path = %q", got)
	}
}
