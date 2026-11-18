// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// SchemaVersion is the result schema version. New optional fields keep it at 1.
const SchemaVersion uint32 = 1

// ClockSyncAssumption is what the run's times rest on: the wall clock does not step, so durations use a monotonic reading.
const ClockSyncAssumption = "the node wall clock does not step during the run"

// RunResult is one run's complete, authoritative result, written atomically to
// result.json on the journal PVC. F1 fills the skeleton fields; the rest come later.
type RunResult struct {
	SchemaVersion uint32            `json:"schemaVersion"`
	Test          string            `json:"test"`
	RunID         string            `json:"runId"`
	ExecutionID   string            `json:"executionId"`
	DryRun        *bool             `json:"dryRun"`
	Outcome                         // verdict and exitCode, flattened
	Reasons       []string          `json:"reasons"`
	Clock         RunClock          `json:"clock"`
	Lock          LockResult        `json:"lock"`
	Stages        []StageResult     `json:"stages"`
	Invariants    []InvariantResult `json:"invariants"`
	Journal       JournalSummary    `json:"journal"`
	// Result is where this record was persisted on the journal PVC. Claim is
	// empty when the file could not be written.
	Result ResultRef `json:"result"`
	// Detail is the test's own findings, such as the fleet-up readiness checks. Only result.json holds it.
	Detail json.RawMessage `json:"detail,omitempty"`
}

// RunClock is the run's wall times. Deadline is the end of the test window, null when none.
type RunClock struct {
	StartedAt           jsonx.Time  `json:"startedAt"`
	Deadline            *jsonx.Time `json:"deadline"`
	EndedAt             jsonx.Time  `json:"endedAt"`
	ClockSyncAssumption string      `json:"clockSyncAssumption"`
}

// LockResult is the site lock as recorded: Kind and FinalState are plain strings, FinalState null until the release is confirmed.
type LockResult struct {
	Name       string      `json:"name"`
	Kind       string      `json:"kind"`
	AcquiredAt *jsonx.Time `json:"acquiredAt"`
	LostAt     *jsonx.Time `json:"lostAt"`
	FinalState *string     `json:"finalState"`
}

// JournalSummary is the journal's counts: a nil pointer is unknown, never 0, and the zero value is all unknown.
type JournalSummary struct {
	Ops      *uint64 `json:"ops"`
	Attempts *uint64 `json:"attempts"`
	Unclear  *uint64 `json:"unclear"`
}

// NoneWritten is the counts of a run that wrote no journal: all 0.
func NoneWritten() JournalSummary {
	z := func() *uint64 { n := uint64(0); return &n }
	return JournalSummary{z(), z(), z()}
}

// ResultRef is where the authoritative result.json is on the PVC. Claim is empty when the file was not written.
type ResultRef struct {
	Claim string `json:"claim"`
	Path  string `json:"path"`
}

// NoRef never points at a result file that does not exist.
func NoRef() ResultRef { return ResultRef{} }

// ResultFile is <dir>/<runId>.result.json: the one authoritative record of the run, replaced atomically on every write.
type ResultFile struct{ Path string }

// ResultFor is the result file of run in dir.
func ResultFor(dir, run string) ResultFile {
	return ResultFile{filepath.Join(dir, run+".result.json")}
}

// Write serializes value and writes it. A value that does not serialize, such as one with a non-finite number, is an
// error and nothing is written.
func (f ResultFile) Write(value any) error {
	data, err := jsonx.Encode(value)
	if err != nil {
		return err
	}
	return f.WriteBytes(data)
}

// WriteBytes replaces the file with data atomically: write a temp file beside it, fsync it, rename it over the target,
// then fsync the directory. A reader sees either the old whole file or the new one, never a partial write.
func (f ResultFile) WriteBytes(data []byte) error {
	tmp := f.Path + ".tmp"
	w, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, err := w.Write(data); err != nil {
		_ = w.Close()
		return err
	}
	if err := w.Sync(); err != nil {
		_ = w.Close()
		return err
	}
	if err := w.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmp, f.Path); err != nil {
		return err
	}
	return syncDir(filepath.Dir(f.Path))
}

func syncDir(dir string) error {
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	return errors.Join(d.Sync(), d.Close())
}
