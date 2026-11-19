// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package result holds a run's verdict and its two written forms: the complete RunResult, written atomically to
// result.json on the journal PVC, and the small Status in the result ConfigMap, whose claim and CAS write it owns.
package result

import "encoding/json"

// Verdict is how a run ended, as the result JSON names it.
type Verdict string

const (
	Passed      Verdict = "passed"     // completed cleanly
	Failed      Verdict = "failed"     // completed, not clean
	Unresolved  Verdict = "unresolved" // completed with unknown state or times
	Go          Verdict = "go"         // a dry or read-only run found the site ready
	NotReady    Verdict = "notReady"
	Refused     Verdict = "refused" // did not run: bad config or a refused lock
	SoftStopped Verdict = "softStopped"
	Aborted     Verdict = "aborted" // lock lost or another hard stop
)

// ExitNotRun is the exit code of a run that did not happen or was stopped.
const ExitNotRun = 2

// ExitCode is 0 for a clean pass or go, 1 for a completed-but-not-clean run, ExitNotRun for one that did not run or was stopped.
func ExitCode(v Verdict) int {
	switch v {
	case Passed, Go:
		return 0
	case Failed, Unresolved:
		return 1
	default:
		return ExitNotRun
	}
}

// Outcome is a verdict and the exit code it gives, never set apart. It flattens into RunResult and Status. Reasons,
// Detail, Stages and Invariants are what a test found; the runner moves them into the RunResult, so they are never
// written from here.
type Outcome struct {
	Verdict    Verdict           `json:"verdict"`
	ExitCode   int               `json:"exitCode"`
	Reasons    []string          `json:"-"`
	Detail     json.RawMessage   `json:"-"`
	Stages     []StageResult     `json:"-"`
	Invariants []InvariantResult `json:"-"`
}

func OutcomeOf(v Verdict) Outcome { return Outcome{Verdict: v, ExitCode: ExitCode(v)} }

// Found is the outcome of a test that reports why it reached v and its findings as JSON.
func Found(v Verdict, reasons []string, detail json.RawMessage) Outcome {
	o := OutcomeOf(v)
	o.Reasons, o.Detail = reasons, detail
	return o
}

// IsSuccess is a verdict a clean run reached.
func IsSuccess(v Verdict) bool { return v == Passed || v == Go }

// Lost downgrades a run that lost the lock to Aborted, unless it never ran.
func Lost(v Verdict) Verdict {
	switch v {
	case Passed, Go, Failed, Unresolved, SoftStopped:
		return Aborted
	default:
		return v
	}
}

// NotClean downgrades a success whose result or release could not be confirmed to Unresolved; a non-success stays.
func NotClean(v Verdict) Verdict {
	if IsSuccess(v) {
		return Unresolved
	}
	return v
}

// StageStatus is a planned stage's admitted status.
type StageStatus string

const (
	StageDone           StageStatus = "done"
	StageFailed         StageStatus = "failed"
	StageDeadlinePassed StageStatus = "deadlinePassed"
	StageUnresolved     StageStatus = "unresolved"
	StageNotApplicable  StageStatus = "notApplicable"
)

// StageID names a planned stage.
type StageID string

// StageResult is one admitted stage. F1 carries the id, status and counts; timing and named units arrive later.
type StageResult struct {
	ID     StageID     `json:"id"`
	Status StageStatus `json:"status"`
	Units  UnitCounts  `json:"units"`
}

// UnitCounts holds every unit in N: unknown = n - done - failed. FailedPct is null when N is 0, never 0.
type UnitCounts struct {
	N         uint64   `json:"n"`
	Done      uint64   `json:"done"`
	Failed    uint64   `json:"failed"`
	Unknown   uint64   `json:"unknown"`
	FailedPct *float64 `json:"failedPct"`
}

// UnitCountsOf keeps every unit in N. Proven failed units are never clipped away: when over N, done gives way.
func UnitCountsOf(n, done, failed uint64) UnitCounts {
	failed = min(failed, n)
	done = min(done, n-failed)
	var pct *float64
	if n > 0 {
		p := 100 * float64(failed) / float64(n)
		pct = &p
	}
	return UnitCounts{n, done, failed, n - done - failed, pct}
}

// InvariantResult is one invariant's outcome over the run's fresh sweeps.
type InvariantResult struct {
	Name   string  `json:"name"`
	Held   bool    `json:"held"`
	Checks uint64  `json:"checks"`
	Detail *string `json:"detail"`
}

// Admitted is what admission made of a test's returned stage results. F1 defines it and the pure Evaluate; the
// producer (the observer against a RunPlan) wires in later.
type Admitted struct {
	Stages     []StageResult // one per planned stage, in plan order
	Reasons    []string      // why each rejected or missing result was; empty when every planned stage had one
	Failed     bool          // a dropped unplanned result was a failure
	Planned    int           // the number of stages in the plan: Stages has one each
	Invariants []string      // the plan's invariants by name: each needs a result
}

// NoStages is the admission of a plan with no stages.
func NoStages() Admitted { return Admitted{} }

// Complete is true when nothing was rejected or missing and every planned stage has its result.
func (a Admitted) Complete() bool { return len(a.Reasons) == 0 && len(a.Stages) == a.Planned }

// effective is the status a result stands for: a positive failed count is a failure, whatever the status says.
func effective(r StageResult) StageStatus {
	if r.Units.Failed > 0 && !failing(r.Status) {
		return StageFailed
	}
	return r.Status
}

func failing(s StageStatus) bool { return s == StageFailed || s == StageDeadlinePassed }

// consistent is true when the counts account for exactly N units and neither done nor failed exceeds N. A malformed
// count (such as done > n) is never trusted as a clean pass.
func consistent(u UnitCounts) bool {
	return u.Done <= u.N && u.Failed <= u.N && u.Done+u.Failed+u.Unknown == u.N
}

// passed is true only for a whitelisted clean stage: a done stage with every unit accounted for and none unknown, or a
// not-applicable stage with no units. Any other status, an unknown unit or a malformed count is not a pass.
func passed(r StageResult) bool {
	switch effective(r) {
	case StageDone:
		return consistent(r.Units) && r.Units.Unknown == 0
	case StageNotApplicable:
		return consistent(r.Units) && r.Units.N == 0
	default:
		return false
	}
}

// Evaluate is the verdict of a completed run. A broken invariant, a failed stage (positive failed count) or a failed
// unplanned result fails it; else a non-whitelisted stage, an unchecked or missing invariant, or an incomplete
// admission is unresolved. Passed needs every stage clean.
func Evaluate(a Admitted, invariants []InvariantResult) Verdict {
	failed := a.Failed
	unchecked := false
	for _, i := range invariants {
		failed = failed || (i.Checks > 0 && !i.Held)
		unchecked = unchecked || i.Checks == 0
	}
	for _, name := range a.Invariants {
		unchecked = unchecked || !hasInvariant(invariants, name)
	}
	unresolved := false
	for _, s := range a.Stages {
		switch {
		case failing(effective(s)):
			failed = true
		case !passed(s):
			unresolved = true
		}
	}
	switch {
	case failed:
		return Failed
	case unchecked || unresolved || !a.Complete():
		return Unresolved
	default:
		return Passed
	}
}

func hasInvariant(invariants []InvariantResult, name string) bool {
	for _, i := range invariants {
		if i.Name == name {
			return true
		}
	}
	return false
}
