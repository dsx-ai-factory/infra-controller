// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

import (
	"reflect"
	"testing"
)

func ptr[T any](v T) *T { return &v }

func stage(status StageStatus, n, done, failed uint64) StageResult {
	return StageResult{ID: "host.endToEnd", Status: status, Units: UnitCountsOf(n, done, failed)}
}

// raw keeps the counts exactly as given, so a malformed accounting (such as
// done > n) reaches Evaluate instead of being normalized by UnitCountsOf.
func raw(status StageStatus, u UnitCounts) StageResult {
	return StageResult{ID: "host.endToEnd", Status: status, Units: u}
}

func inv(name string, held bool, checks uint64) InvariantResult {
	return InvariantResult{Name: name, Held: held, Checks: checks}
}

// TestEvaluate proves Passed needs a complete admission with nothing failed, nothing unresolved and every invariant
// checked: an unknown state is never Passed.
func TestEvaluate(t *testing.T) {
	done := []StageResult{stage(StageDone, 10, 10, 0)}
	checked := []InvariantResult{inv("links", true, 3)}
	rows := []struct {
		name       string
		admitted   Admitted
		invariants []InvariantResult
		want       Verdict
	}{
		{"clean pass", Admitted{Stages: done, Planned: 1, Invariants: []string{"links"}}, checked, Passed},
		{"no stages, no invariants, passes", NoStages(), nil, Passed},
		{"broken invariant fails", Admitted{Stages: done, Planned: 1, Invariants: []string{"links"}}, []InvariantResult{inv("links", false, 3)}, Failed},
		{"not held but zero checks is unresolved, not failed", Admitted{Stages: done, Planned: 1, Invariants: []string{"links"}}, []InvariantResult{inv("links", false, 0)}, Unresolved},
		{"failed stage fails", Admitted{Stages: []StageResult{stage(StageFailed, 10, 9, 1)}, Planned: 1}, nil, Failed},
		{"done status with a failed count fails", Admitted{Stages: []StageResult{stage(StageDone, 10, 9, 1)}, Planned: 1}, nil, Failed},
		{"dropped unplanned failure fails", Admitted{Stages: done, Planned: 1, Failed: true}, checked, Failed},
		{"invariant never checked is unresolved", Admitted{Stages: done, Planned: 1, Invariants: []string{"links"}}, []InvariantResult{inv("links", true, 0)}, Unresolved},
		{"missing invariant result is unresolved", Admitted{Stages: done, Planned: 1, Invariants: []string{"links"}}, nil, Unresolved},
		{"unresolved stage is unresolved", Admitted{Stages: []StageResult{stage(StageUnresolved, 10, 0, 0)}, Planned: 1}, nil, Unresolved},
		{"a rejected result (reason) is unresolved", Admitted{Stages: done, Reasons: []string{"stage x rejected"}, Planned: 1}, checked, Unresolved},
		{"a missing planned stage is unresolved", Admitted{Stages: done, Planned: 2}, checked, Unresolved},
		{"not applicable counts as done", Admitted{Stages: []StageResult{stage(StageNotApplicable, 0, 0, 0)}, Planned: 1}, checked, Passed},
		{"done with an unknown unit is unresolved, not passed", Admitted{Stages: []StageResult{stage(StageDone, 10, 9, 0)}, Planned: 1}, checked, Unresolved},
		{"an unknown stage status is unresolved, not passed", Admitted{Stages: []StageResult{stage(StageStatus("surprise"), 10, 10, 0)}, Planned: 1}, checked, Unresolved},
		{"an empty stage status is unresolved, not passed", Admitted{Stages: []StageResult{stage(StageStatus(""), 10, 10, 0)}, Planned: 1}, checked, Unresolved},
		{"a malformed count (done over n) is unresolved, not passed", Admitted{Stages: []StageResult{raw(StageDone, UnitCounts{N: 10, Done: 15})}, Planned: 1}, checked, Unresolved},
		{"an unknown status with a proven failure still fails", Admitted{Stages: []StageResult{stage(StageStatus("surprise"), 10, 9, 1)}, Planned: 1}, checked, Failed},
		{"not applicable with units is unresolved", Admitted{Stages: []StageResult{raw(StageNotApplicable, UnitCounts{N: 3, Done: 3})}, Planned: 1}, checked, Unresolved},
		{"not applicable with zero N but an unknown unit is unresolved", Admitted{Stages: []StageResult{raw(StageNotApplicable, UnitCounts{N: 0, Unknown: 1})}, Planned: 1}, checked, Unresolved},
	}
	for _, r := range rows {
		if got := Evaluate(r.admitted, r.invariants); got != r.want {
			t.Errorf("%s: Evaluate = %q, want %q", r.name, got, r.want)
		}
	}
}

func TestExitCode(t *testing.T) {
	rows := []struct {
		v    Verdict
		want int
	}{
		{Passed, 0}, {Go, 0}, {Failed, 1}, {Unresolved, 1},
		{NotReady, 2}, {Refused, 2}, {SoftStopped, 2}, {Aborted, 2},
	}
	for _, r := range rows {
		if got := ExitCode(r.v); got != r.want {
			t.Errorf("ExitCode(%q) = %d, want %d", r.v, got, r.want)
		}
		if o := OutcomeOf(r.v); o.ExitCode != r.want {
			t.Errorf("OutcomeOf(%q).ExitCode = %d, want %d", r.v, o.ExitCode, r.want)
		}
	}
}

func TestUnitCounts(t *testing.T) {
	rows := []struct {
		name            string
		n, done, failed uint64
		want            UnitCounts
	}{
		{"all done", 10, 10, 0, UnitCounts{10, 10, 0, 0, ptr(0.0)}},
		{"some failed, some unknown", 10, 5, 3, UnitCounts{10, 5, 3, 2, ptr(30.0)}},
		{"N 0 has a null pct", 0, 0, 0, UnitCounts{0, 0, 0, 0, nil}},
		{"more done than N, one failed", 4, 6, 1, UnitCounts{4, 3, 1, 0, ptr(25.0)}},
		{"all done and one failed, the failure stays", 10, 10, 1, UnitCounts{10, 9, 1, 0, ptr(10.0)}},
		{"more failed than N", 4, 3, 5, UnitCounts{4, 0, 4, 0, ptr(100.0)}},
	}
	for _, r := range rows {
		if got := UnitCountsOf(r.n, r.done, r.failed); !reflect.DeepEqual(got, r.want) {
			t.Errorf("%s: got %+v, want %+v", r.name, got, r.want)
		}
	}
}

func TestDowngrades(t *testing.T) {
	for _, v := range []Verdict{Passed, Go, Failed, Unresolved, SoftStopped} {
		if Lost(v) != Aborted {
			t.Errorf("Lost(%q) = %q, want aborted", v, Lost(v))
		}
	}
	for _, v := range []Verdict{NotReady, Refused, Aborted} {
		if Lost(v) != v {
			t.Errorf("Lost(%q) changed a never-ran verdict", v)
		}
	}
	if NotClean(Passed) != Unresolved || NotClean(Go) != Unresolved {
		t.Error("NotClean must downgrade a success to unresolved")
	}
	if NotClean(Failed) != Failed || NotClean(Aborted) != Aborted {
		t.Error("NotClean must leave a non-success as it is")
	}
}
