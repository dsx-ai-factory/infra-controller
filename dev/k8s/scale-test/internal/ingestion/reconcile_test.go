// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"fmt"
	"slices"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
)

// site is the fixture with two DPUs under host AA:00 and its counts, served by one MAT pod at a nico-api URL.
func site() *inventory.SiteInventory {
	s := fixture()
	s.Pods[0].Records.CarbideAPIURL = "https://nico-api:1079"
	s.Pods[0].Records.DPUs = []inventory.DPU{{BmcMac: "dd:00", ParentBmcMac: "AA:00"}, {BmcMac: "dd:01", ParentBmcMac: "AA:00"}}
	s.N = inventory.TypeCounts{RackGroup: 2, Rack: 2, Host: 2, DPU: 2, Switch: 1, PowerShelf: 1}
	return s
}

func unit(t probeapi.UnitType, id string, stage probeapi.Stage) probeapi.Unit {
	u := probeapi.Unit{ID: id, Type: t, Source: probeapi.SourceNicoAPI, Stage: stage}
	if t == probeapi.UnitTypeHost {
		u.Host = &probeapi.HostFields{Allocatable: new(true)}
	}
	return u
}

func record(r probeapi.RecordType, id, mac, device string) probeapi.Unit {
	u := unit(probeapi.UnitTypeExpectedRecord, id, probeapi.StageExpected)
	u.Expected = &probeapi.ExpectedFields{RecordType: r, Linked: device != "", DeviceID: &device}
	if mac != "" {
		u.Expected.BmcMac = &mac
	}
	return u
}

// ingested is the end state of a clean run of inv: each record linked to its Ready device ("dev-<mac>", a rack by its ID), each DPU Ready under its host.
func ingested(inv *inventory.SiteInventory) []probeapi.Unit {
	var out []probeapi.Unit
	types := map[inventory.DeviceKind][2]string{inventory.KindMachine: {"host", "host"}, inventory.KindSwitch: {"switch", "switch"},
		inventory.KindPowerShelf: {"power_shelf", "power_shelf"}}
	for _, p := range inv.Pods {
		for _, r := range p.Records.Racks {
			out = append(out, record(probeapi.RecordTypeRack, r.RackID, "", r.RackID), unit(probeapi.UnitTypeRack, r.RackID, probeapi.StageReady))
		}
		for _, d := range p.Records.Devices {
			tt := types[d.Kind]
			out = append(out, record(probeapi.RecordType(tt[0]), "rec-"+d.BmcMac, d.BmcMac, "dev-"+d.BmcMac), unit(probeapi.UnitType(tt[1]), "dev-"+d.BmcMac, probeapi.StageReady))
		}
		for _, d := range p.Records.DPUs {
			u := unit(probeapi.UnitTypeDpu, "dpu-"+d.BmcMac, probeapi.StageReady)
			u.Dpu = &probeapi.DpuFields{HostID: new("dev-" + d.ParentBmcMac)}
			out = append(out, u)
		}
	}
	return out
}

// endOf is units as the nine end reads would serve them, every read answered.
func endOf(units []probeapi.Unit) EndUnits {
	end := EndUnits{Records: map[probeapi.RecordType]endRead{}, Devices: map[probeapi.UnitType]endRead{}}
	for _, k := range kinds {
		end.Records[k.record], end.Devices[k.device] = endRead{units: []probeapi.Unit{}}, endRead{units: []probeapi.Unit{}}
	}
	end.Devices[probeapi.UnitTypeDpu] = endRead{units: []probeapi.Unit{}}
	for _, u := range units {
		if u.Expected != nil {
			r := end.Records[u.Expected.RecordType]
			end.Records[u.Expected.RecordType] = endRead{units: append(r.units, u)}
		} else {
			end.Devices[u.Type] = endRead{units: append(end.Devices[u.Type].units, u)}
		}
	}
	return end
}

func allSent(plan WritePlan) map[WriteKey]KeyOutcome {
	out := map[WriteKey]KeyOutcome{}
	for _, k := range plan.keys() {
		out[k] = KeyOutcome{Status: Sent}
	}
	return out
}

// find is the first unit of units with id, to change in place.
func find(units []probeapi.Unit, id string) *probeapi.Unit {
	return &units[slices.IndexFunc(units, func(u probeapi.Unit) bool { return u.ID == id })]
}

// TestReconcileRows proves each join and send-outcome boundary on one planned unit, and what each end state reports as unexpected or pending.
func TestReconcileRows(t *testing.T) {
	inv := site()
	plan, _, err := Records(inv)
	if err != nil {
		t.Fatal(err)
	}
	const host, dpu0, dpu1 = "host/aa:00", "host/aa:00#0", "host/aa:00#1"
	inError := func(id string) func([]probeapi.Unit) []probeapi.Unit {
		return func(u []probeapi.Unit) []probeapi.Unit { find(u, id).Stage = probeapi.StageError; return u }
	}
	for _, tc := range []struct {
		name   string
		change func([]probeapi.Unit) []probeapi.Unit
		sent   KeyOutcome
		read   probeapi.UnitType // a device read that fails
		stage  result.StageID
		unit   string
		want   Outcome
		report string // a line of Unexpected or Pending, or none
	}{
		{"clean", nil, KeyOutcome{Status: Sent}, "", HostEndToEnd, host, done, ""},
		{"a linked device missing", func(u []probeapi.Unit) []probeapi.Unit {
			return slices.DeleteFunc(u, func(x probeapi.Unit) bool { return x.ID == "dev-AA:00" && x.Expected == nil })
		}, KeyOutcome{Status: Sent}, "", HostEndToEnd, host, unproven(causeNoDevice), ""},
		{"two devices with one ID", func(u []probeapi.Unit) []probeapi.Unit { return append(u, *find(u, "dev-AA:00")) },
			KeyOutcome{Status: Sent}, "", HostEndToEnd, host, unproven(causeAmbiguous), "more than one host unit dev-AA:00"},
		{"an unclear send whose end evidence is complete", nil, KeyOutcome{Status: Unclear}, "", HostEndToEnd, host, done, ""},
		{"an unclear send with no record", func(u []probeapi.Unit) []probeapi.Unit {
			return slices.DeleteFunc(u, func(x probeapi.Unit) bool { return x.ID == "rec-AA:00" })
		}, KeyOutcome{Status: Unclear}, "", HostReadyDiagnostic, host, unproven(causeNoReply), "host unit dev-AA:00 is not linked by a planned record"},
		{"a rejected key whose device shows in error", inError("dev-AA:00"), KeyOutcome{Status: Rejected, Reason: "grpc-InvalidArgument"}, "",
			HostEndToEnd, host, failedAs("grpc-InvalidArgument", true), ""},
		{"a rejected key in discovery is outside the filter", nil, KeyOutcome{Status: Rejected, Reason: "grpc-InvalidArgument"}, "",
			HostDiscovery, host, failedAs("grpc-InvalidArgument", false), ""},
		{"a key never sent", nil, KeyOutcome{Status: NotSent}, "", HostDiscovery, host, unproven(causeNotSent), ""},
		{"a device not Ready", func(u []probeapi.Unit) []probeapi.Unit {
			find(u, "dev-AA:00").Stage = probeapi.StageRegistered
			return u
		},
			KeyOutcome{Status: Sent}, "", HostReadyDiagnostic, host, notYet(causeNotReady), ""},
		{"a Ready host not allocatable", func(u []probeapi.Unit) []probeapi.Unit { find(u, "dev-AA:00").Host.Allocatable = new(false); return u },
			KeyOutcome{Status: Sent}, "", HostEndToEnd, host, notYet(causeNotAllocatable), ""},
		{"a Ready host whose allocatability is unknown", func(u []probeapi.Unit) []probeapi.Unit { find(u, "dev-AA:00").Host.Allocatable = nil; return u },
			KeyOutcome{Status: Sent}, "", HostEndToEnd, host, notYet(causeAllocatableUnknown), ""},
		{"a host read that failed", nil, KeyOutcome{Status: Sent}, probeapi.UnitTypeHost, HostEndToEnd, host, unproven(causeUnread), ""},
		{"a record not linked yet, its host already swept", func(u []probeapi.Unit) []probeapi.Unit { find(u, "rec-AA:00").Expected.Linked = false; return u },
			KeyOutcome{Status: Sent}, "", HostDiscovery, host, notYet(causeNotLinked), "host unit dev-AA:00 is not linked by a planned record"},
		{"DPUs of a host not linked yet", func(u []probeapi.Unit) []probeapi.Unit { find(u, "rec-AA:00").Expected.Linked = false; return u },
			KeyOutcome{Status: Sent}, "", DPUEndToEnd, dpu1, notYet(causeParentNotLinked), "host unit dev-AA:00 is not linked by a planned record"},
		{"two records linking one host", func(u []probeapi.Unit) []probeapi.Unit {
			find(u, "rec-aa:01").Expected.DeviceID = new("dev-AA:00")
			return u
		},
			KeyOutcome{Status: Sent}, "", HostEndToEnd, host, unproven(causeAmbiguous), "host unit dev-AA:00 is linked by 2 expected records: [host/aa:00 host/aa:01]"},
		{"a record not planned", func(u []probeapi.Unit) []probeapi.Unit {
			return append(u, record(probeapi.RecordTypeHost, "rec-ff", "ff", ""))
		},
			KeyOutcome{Status: Sent}, "", HostEndToEnd, host, done, "expected record host/ff is not planned"},
		{"a rack no planned rack has", func(u []probeapi.Unit) []probeapi.Unit {
			return append(u, unit(probeapi.UnitTypeRack, "r9", probeapi.StageReady))
		},
			KeyOutcome{Status: Sent}, "", HostEndToEnd, host, done, "rack unit r9 is not linked by a planned record"},
		{"a surplus DPU in error comes first and is never done", func(u []probeapi.Unit) []probeapi.Unit {
			x := unit(probeapi.UnitTypeDpu, "dpu-x", probeapi.StageError)
			x.Dpu = &probeapi.DpuFields{HostID: new("dev-AA:00")}
			return append(u, x)
		}, KeyOutcome{Status: Sent}, "", DPUEndToEnd, dpu0, failedAs(causeError, true), "host host/aa:00 lists 3 DPUs, 2 planned"},
		{"DPUs of a rejected host, one in error", inError("dpu-dd:01"), KeyOutcome{Status: Rejected, Reason: "grpc-InvalidArgument"}, "",
			DPUEndToEnd, dpu0, failedAs(causeParentNotAccepted, true), ""},
		{"DPUs of a rejected host past the visible errors", inError("dpu-dd:01"), KeyOutcome{Status: Rejected}, "",
			DPUEndToEnd, dpu1, failedAs(causeParentNotAccepted, false), ""},
		{"DPUs of a host never sent", nil, KeyOutcome{Status: NotSent}, "", DPUEndToEnd, dpu0, unproven(causeNotSent), ""},
	} {
		units := ingested(inv)
		if tc.change != nil {
			units = tc.change(units)
		}
		end, sent := endOf(units), allSent(plan)
		if tc.read != "" {
			end.Devices[tc.read] = endRead{err: "probe: timeout"}
		}
		sent[host] = tc.sent
		got := reconcile(plan, inv, sent, end)
		is(t, got.Units[tc.stage][tc.unit], tc.want, tc.name)
		reports := append(slices.Clone(got.Unexpected), got.Pending...)
		if tc.report == "" && len(reports) > 0 || tc.report != "" && !slices.Contains(reports, tc.report) {
			t.Errorf("%s: unexpected %q, pending %q; want %q", tc.name, got.Unexpected, got.Pending, tc.report)
		}
		for id, tally := range got.Tallies {
			if n := map[result.StageID]uint64{DPUEndToEnd: 2, HostDiscovery: 2, HostEndToEnd: 2, HostReadyDiagnostic: 2}[id]; n > 0 &&
				tally.Done+tally.Failed+tally.FailedOutsideFilter+tally.Unknown != n {
				t.Errorf("%s: %s tally %+v does not hold N %d", tc.name, id, *tally, n)
			}
		}
	}
}

// TestUnplannedDevicesBeyondTheOpenKeysAreExtra: a device no planned record links is pending while an open planned key could still explain it, and the units
// beyond that room are a proven excess.
func TestUnplannedDevicesBeyondTheOpenKeysAreExtra(t *testing.T) {
	inv := site()
	plan, _, _ := Records(inv)
	units := slices.DeleteFunc(ingested(inv), func(u probeapi.Unit) bool { return u.ID == "rec-aa:01" })
	units = append(units, unit(probeapi.UnitTypeHost, "stray", probeapi.StageReady))
	got := reconcile(plan, inv, allSent(plan), endOf(units))
	is(t, []any{got.Unexpected, len(got.Pending)}, []any{[]string{"2 host units the plan does not account for, room for 1: 1 extra"}, 2}, "one open key, two hosts")
	is(t, unexpectedUnits(got).Held, false, "a proven excess breaks unexpectedUnits")
}

func summary(seconds ...*float64) *probeapi.SummaryResponse {
	s := &probeapi.SummaryResponse{Family: probeapi.FamilyHost, TimingErrorSeconds: new(3.0), MaxSweepGapSeconds: new(1.0), FinalScrapeSeen: true}
	for i, q := range probeapi.FixedQuantiles {
		s.Crossings = append(s.Crossings, probeapi.Crossing{Q: q, Seconds: seconds[min(i, len(seconds)-1)]})
	}
	return s
}

// TestTimingAndStageStatus: the timing a summary proves, and the stage status as the more severe of it and the planned cohort. Unknown is never done; a
// stage that never completes is a deadline passed or unresolved, never done.
func TestTimingAndStageStatus(t *testing.T) {
	staleScrape := summary(new(1.0), nil)
	staleScrape.FinalScrapeSeen = false
	noError := summary(new(1.0))
	noError.TimingErrorSeconds = nil
	for _, tc := range []struct {
		name string
		s    *probeapi.SummaryResponse
		want result.StageStatus
	}{
		{"no summary", nil, result.StageUnresolved},
		{"every crossing timed", summary(new(1.0)), result.StageDone},
		{"no timing error", noError, result.StageUnresolved},
		{"a crossing never reached", summary(new(1.0), nil), result.StageDeadlinePassed},
		{"a crossing not reached before the final scrape", staleScrape, result.StageUnresolved},
		{"crossings missing", &probeapi.SummaryResponse{TimingErrorSeconds: new(1.0), MaxSweepGapSeconds: new(1.0)}, result.StageUnresolved},
	} {
		is(t, timing(tc.s), tc.want, tc.name)
	}
	st := Stage{ID: HostDiscovery, N: 2}
	for _, tc := range []struct {
		name   string
		tally  *Tally
		timing result.StageStatus
		want   result.StageStatus
		units  result.UnitCounts
	}{
		{"both done", &Tally{Done: 2}, result.StageDone, result.StageDone, result.UnitCountsOf(2, 2, 0)},
		{"timing unresolved stays", &Tally{Done: 2}, result.StageUnresolved, result.StageUnresolved, result.UnitCountsOf(2, 2, 0)},
		{"deadline passed stays", &Tally{Done: 2}, result.StageDeadlinePassed, result.StageDeadlinePassed, result.UnitCountsOf(2, 2, 0)},
		{"a status outside the order", &Tally{Done: 2}, "", result.StageUnresolved, result.UnitCountsOf(2, 2, 0)},
		{"an unknown key is not done", &Tally{Done: 1, Unknown: 1, Unproven: 1}, result.StageDone, result.StageUnresolved, result.UnitCountsOf(2, 1, 0)},
		{"the deadline is worse than unknown", &Tally{Done: 1, Unknown: 1}, result.StageDeadlinePassed, result.StageDeadlinePassed, result.UnitCountsOf(2, 1, 0)},
		{"a failed key outside the filter", &Tally{Done: 1, FailedOutsideFilter: 1}, result.StageDone, result.StageFailed, result.UnitCountsOf(2, 1, 1)},
		{"a count gap", &Tally{Done: 1}, result.StageDone, result.StageUnresolved, result.UnitCountsOf(2, 1, 0)},
		{"no per-key evidence", nil, result.StageDone, result.StageUnresolved, result.UnitCountsOf(2, 0, 0)},
	} {
		got := stageResult(st, tc.tally, tc.timing)
		is(t, []any{got.Status, got.Units}, []any{tc.want, tc.units}, tc.name)
	}
	is(t, stageResult(Stage{ID: DPUEndToEnd}, &Tally{Done: 3}, result.StageDone).Status, result.StageNotApplicable, "a zero-N stage")
	for _, s := range Stages(inventory.TypeCounts{}) {
		if r := stageResult(s, nil, result.StageUnresolved); r.Status != result.StageNotApplicable || r.Units != result.UnitCountsOf(0, 0, 0) {
			t.Errorf("an empty inventory: %+v", r)
		}
	}
}

// TestInvariantResults: a break the observer saw stands, an end read off the baseline breaks, and a failed end read, a run that never checked, a failed pod
// capture or no pod leave an invariant unresolved, never held.
func TestInvariantResults(t *testing.T) {
	inv := countInvariant{Name: "ambiguousLinks"}
	seen := func(checks uint64, brk string) observed {
		o := observed{Breaks: map[string]string{}, Checks: map[string]uint64{"ambiguousLinks": checks}, Unread: map[string]string{}}
		if brk != "" {
			o.Breaks["ambiguousLinks"] = brk
		}
		return o
	}
	pods, none := func(uid string, n uint32) map[string]podState { return map[string]podState{"a/p": {uid, n}} }, map[string]podState{}
	text := func(r result.InvariantResult) string {
		detail := ""
		if r.Detail != nil {
			detail = *r.Detail
		}
		return fmt.Sprintf("held=%v checks=%d %s", r.Held, r.Checks, detail)
	}
	for _, tc := range []struct {
		name string
		got  result.InvariantResult
		want string
	}{
		{"held", countResult(inv, seen(2, ""), 0, ""), "held=true checks=3 "},
		{"a break seen stands over a clean end", countResult(inv, seen(2, "count 1 during the run, 0 before it"), 0, ""), "held=false checks=2 count 1 during the run, 0 before it"},
		{"a break seen stands over a cut end", countResult(inv, seen(1, "x"), 0, errPastReadEnd.Error()), "held=false checks=1 x"},
		{"an end read off the baseline", countResult(inv, seen(2, ""), 1, ""), "held=false checks=3 count 1 at the end, 0 before the run"},
		{"a cut end read", countResult(inv, seen(2, ""), 0, errReadsCancelled.Error()), "held=true checks=0 unavailable: end read: the run was cancelled during the end reads"},
		{"never checked", countResult(inv, seen(0, ""), 0, ""), "held=true checks=0 unavailable: never checked during the run: never read"},
		{"pods unchanged", restartResult("m", pods("u1", 1), pods("u1", 1), ""), "held=true checks=1 "},
		{"a restart", restartResult("m", pods("u1", 1), pods("u1", 2), ""), "held=false checks=1 pod a/p restarts 1 to 2"},
		{"a pod replaced under its name", restartResult("m", pods("u1", 1), pods("u2", 1), ""), "held=false checks=1 pod a/p replaced"},
		{"a renamed pod", restartResult("m", pods("u1", 1), map[string]podState{"a/q": {}}, ""), "held=false checks=1 pod a/p removed; pod a/q added"},
		{"a failed capture proves no removal", restartResult("m", pods("u1", 1), none, "listing x in a: timeout"),
			"held=true checks=0 unavailable: listing x in a: timeout"},
		{"no pod to watch", restartResult("m", none, none, ""), "held=true checks=0 unavailable: no pod was captured"},
		{"unexpected units with a failed read", unexpectedUnits(&Reconciled{ReadErrors: []string{"host units: probe: timeout"}}),
			"held=true checks=0 unavailable: 1 end reads failed, first: host units: probe: timeout"},
	} {
		is(t, text(tc.got), tc.want, tc.name)
	}
}
