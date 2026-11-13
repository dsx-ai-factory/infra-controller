// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"cmp"
	"fmt"
	"maps"
	"slices"
	"strings"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
)

// Reconciliation gives every planned unit of every stage one outcome after the run, from fresh end reads joined to the planned keys and their send outcomes: an
// aggregate count never stands in for the planned cohort. A failed unit is in the stage's failed filter (its device shows in the error stage) or belongs to a key
// never accepted. An unknown unit is unproven when its evidence is missing: a failed or stale read, no record, no device, or a join that is not one to one. The
// site starts empty, so every device must be linked by one planned record and every DPU listed by such a host; one that is not is extra only when no open planned
// key could explain it, else pending: the families are swept apart.

// Causes of units that are not done. Each is fixed text: a device's own reason is never repeated.
const (
	causeNotLinked          = "notLinked"
	causeNotReady           = "notReady"
	causeNotAllocatable     = "notAllocatable"
	causeAllocatableUnknown = "allocatableUnknown"
	causeError              = "error"
	causeParentNotAccepted  = "parentNotAccepted"
	causeParentNotLinked    = "parentNotLinked"
	causeUnread             = "unread"
	causeNoRecord           = "noRecord"
	causeNoDevice           = "noDevice"
	causeAmbiguous          = "ambiguous"
	causeNoReply            = "noReply"
	causeNotSent            = "notSent"
)

// Outcome is one planned unit's end in one stage: done, failed (InFilter when the stage's failed filter counts it) or unknown (Unproven when its evidence is
// missing).
type Outcome struct {
	Outcome  string `json:"outcome"`
	Cause    string `json:"cause,omitempty"`
	InFilter bool   `json:"inFilter,omitempty"`
	Unproven bool   `json:"unproven,omitempty"`
}

var done = Outcome{Outcome: "done"}

func notYet(c string) Outcome            { return Outcome{Outcome: "unknown", Cause: c} }
func unproven(c string) Outcome          { return Outcome{Outcome: "unknown", Cause: c, Unproven: true} }
func failedAs(c string, in bool) Outcome { return Outcome{Outcome: "failed", Cause: c, InFilter: in} }

// Tally is one stage's units, disjoint: Done + Failed + FailedOutsideFilter + Unknown is its N. Unproven counts the unknown units without evidence.
type Tally struct {
	Done                uint64 `json:"done"`
	Failed              uint64 `json:"failed"`
	FailedOutsideFilter uint64 `json:"failedOutsideFilter"`
	Unknown             uint64 `json:"unknown"`
	Unproven            uint64 `json:"unproven"`
}

// Reconciled is one outcome per stage and unit, a DPU unit named <host key>#<n> (DPUs are counted per host, not told apart), and what the plan does not account
// for, each list sorted.
type Reconciled struct {
	Units      map[result.StageID]map[string]Outcome
	Tallies    map[result.StageID]*Tally
	ReadErrors []string // end reads that failed or were stale
	Unexpected []string // records and devices the plan does not account for
	Pending    []string // devices no planned record links yet while an open planned key could still explain them
}

func (r *Reconciled) record(stage result.StageID, unit string, o Outcome) {
	if r.Units[stage] == nil {
		r.Units[stage], r.Tallies[stage] = map[string]Outcome{}, &Tally{}
	}
	r.Units[stage][unit] = o
	t := r.Tallies[stage]
	switch {
	case o.Outcome == "done":
		t.Done++
	case o.Outcome == "failed" && o.InFilter:
		t.Failed++
	case o.Outcome == "failed":
		t.FailedOutsideFilter++
	case o.Unproven:
		t.Unknown++
		t.Unproven++
	default:
		t.Unknown++
	}
}

// endRead is one end read: its units, or why it failed. A read not taken has no entry and counts as failed.
type endRead struct {
	units []probeapi.Unit
	err   string
}

// EndUnits is what the end reads found per record type and device type, each from its own fresh snapshot.
type EndUnits struct {
	Records map[probeapi.RecordType]endRead
	Devices map[probeapi.UnitType]endRead
}

func readOK[K comparable](m map[K]endRead, k K) bool { r, in := m[k]; return in && r.err == "" }

// kinds ties each write stream to its record type, device type and discovery and Ready stages.
var kinds = []struct {
	stream         Stream
	record         probeapi.RecordType
	device         probeapi.UnitType
	discover, done result.StageID
}{
	{Rack, probeapi.RecordTypeRack, probeapi.UnitTypeRack, RackDiscovery, RackEndToEnd},
	{Host, probeapi.RecordTypeHost, probeapi.UnitTypeHost, HostDiscovery, HostReadyDiagnostic},
	{Switch, probeapi.RecordTypeSwitch, probeapi.UnitTypeSwitch, SwitchDiscovery, SwitchEndToEnd},
	{PowerShelf, probeapi.RecordTypePowerShelf, probeapi.UnitTypePowerShelf, PowerShelfDiscovery, PowerShelfEndToEnd},
}

type devKey struct {
	t  probeapi.UnitType
	id string
}

// index is the successful reads: records by key, devices by type and ID, and the keys whose records link each device. A key or device more than one unit
// answers for is ambiguous.
type index struct {
	records    map[WriteKey]*probeapi.Unit
	devices    map[devKey]*probeapi.Unit
	dupRecords map[WriteKey]bool
	dupDevices map[devKey]bool
	claims     map[devKey][]WriteKey
}

// recordKey is the write key a record answers for and the device type it links, false for a rack group.
func recordKey(u *probeapi.Unit) (WriteKey, probeapi.UnitType, bool) {
	for _, k := range kinds {
		switch e := u.Expected; {
		case e == nil || e.RecordType != k.record:
		case k.stream == Rack:
			return RackKey(u.ID), k.device, true
		case e.BmcMac != nil:
			return DeviceKey(k.stream, *e.BmcMac), k.device, true
		}
	}
	return "", "", false
}

func newIndex(end EndUnits, out *Reconciled) *index {
	ix := &index{map[WriteKey]*probeapi.Unit{}, map[devKey]*probeapi.Unit{}, map[WriteKey]bool{}, map[devKey]bool{}, map[devKey][]WriteKey{}}
	for t, r := range end.Records {
		if r.err != "" {
			out.ReadErrors = append(out.ReadErrors, fmt.Sprintf("%s expected records: %s", t, r.err))
		}
		for i := range r.units {
			u := &r.units[i]
			key, dt, ok := recordKey(u)
			if !ok {
				continue
			}
			if e := u.Expected; e.Linked && e.DeviceID != nil && *e.DeviceID != "" {
				if d := (devKey{dt, *e.DeviceID}); !slices.Contains(ix.claims[d], key) {
					ix.claims[d] = append(ix.claims[d], key)
				}
			}
			if ix.records[key] != nil {
				ix.dupRecords[key] = true
			}
			ix.records[key] = u
		}
	}
	for t, r := range end.Devices {
		if r.err != "" {
			out.ReadErrors = append(out.ReadErrors, fmt.Sprintf("%s units: %s", t, r.err))
		}
		for i := range r.units {
			d := devKey{t, r.units[i].ID}
			if ix.devices[d] != nil {
				ix.dupDevices[d] = true
			}
			ix.devices[d] = &r.units[i]
		}
	}
	for k := range ix.dupRecords {
		out.Unexpected = append(out.Unexpected, fmt.Sprintf("more than one expected record for %s", k))
	}
	for d := range ix.dupDevices {
		out.Unexpected = append(out.Unexpected, fmt.Sprintf("more than one %s unit %s", d.t, d.id))
	}
	for d, keys := range ix.claims {
		if len(keys) > 1 {
			slices.Sort(keys)
			out.Unexpected = append(out.Unexpected, fmt.Sprintf("%s unit %s is linked by %d expected records: %v", d.t, d.id, len(keys), keys))
		}
	}
	return ix
}

// joined is what the end state says about one planned key, in its discovery, Ready and Ready-and-allocatable stages.
type joined struct {
	out     [3]Outcome
	settled bool   // no record, an ambiguous one or a failed read: no stage can be judged
	open    bool   // settled or not linked: the key may still link a device no planned record links yet
	device  string // the device its record alone links
	inError bool
}

func (ix *index) join(record probeapi.RecordType, device probeapi.UnitType, key WriteKey, end EndUnits) joined {
	settle := func(c string) joined {
		o := unproven(c)
		return joined{out: [3]Outcome{o, o, o}, settled: true, open: true}
	}
	rec := ix.records[key]
	switch {
	case !readOK(end.Records, record):
		return settle(causeUnread)
	case ix.dupRecords[key]:
		return settle(causeAmbiguous)
	case rec == nil:
		return settle(causeNoRecord)
	case !rec.Expected.Linked || rec.Expected.DeviceID == nil || *rec.Expected.DeviceID == "":
		o := notYet(causeNotLinked)
		return joined{out: [3]Outcome{o, o, o}, open: true}
	}
	d := devKey{device, *rec.Expected.DeviceID}
	if len(ix.claims[d]) > 1 { // another key links the same device: neither link proves its key
		return settle(causeAmbiguous)
	}
	j, o := joined{device: d.id}, unproven(causeNoDevice)
	switch dev := ix.devices[d]; {
	case !readOK(end.Devices, device):
		o = unproven(causeUnread)
	case ix.dupDevices[d]:
		o = unproven(causeAmbiguous)
	case dev != nil:
		j.out, j.inError = [3]Outcome{done, deviceOutcome(dev, false), deviceOutcome(dev, true)}, dev.Stage == probeapi.StageError
		return j
	}
	j.out = [3]Outcome{done, o, o}
	return j
}

// deviceOutcome is how a device stands for a Ready stage; allocatable adds the host gate.
func deviceOutcome(d *probeapi.Unit, allocatable bool) Outcome {
	switch {
	case d.Stage == probeapi.StageError:
		return failedAs(causeError, true)
	case d.Stage != probeapi.StageReady:
		return notYet(causeNotReady)
	case !allocatable:
		return done
	case d.Host == nil || d.Host.Allocatable == nil:
		return notYet(causeAllocatableUnknown)
	case *d.Host.Allocatable:
		return done
	}
	return notYet(causeNotAllocatable)
}

// reconcile gives every planned key of every stage one outcome from its send outcome joined to the end state. An unclear key may have landed: the end state
// decides, and without evidence it stays unproven. A key never sent is unproven. A key never accepted is failed, in the failed filter only when its device shows
// in the error stage.
func reconcile(plan WritePlan, inv *inventory.SiteInventory, sent map[WriteKey]KeyOutcome, end EndUnits) *Reconciled {
	out := &Reconciled{Units: map[result.StageID]map[string]Outcome{}, Tallies: map[result.StageID]*Tally{},
		ReadErrors: []string{}, Unexpected: []string{}, Pending: []string{}}
	ix := newIndex(end, out)
	planned := map[WriteKey]bool{}
	for _, k := range plan.UnitKeys() {
		planned[k] = true
	}
	for k := range ix.records {
		if !planned[k] {
			out.Unexpected = append(out.Unexpected, fmt.Sprintf("expected record %s is not planned", k))
		}
	}
	hosts, open := map[WriteKey]string{}, map[probeapi.UnitType]int{}
	for _, k := range kinds {
		for _, key := range plan.UnitKeys() {
			if !strings.HasPrefix(string(key), string(k.stream)+"/") {
				continue
			}
			j := ix.join(k.record, k.device, key, end)
			if j.open {
				open[k.device]++
			}
			if k.stream == Host {
				hosts[key] = j.device
			}
			o := j.out
			switch s := sent[key]; s.Status {
			case Sent:
			case Unclear:
				if j.settled {
					u := unproven(causeNoReply)
					o = [3]Outcome{u, u, u}
				}
			case NotSent, Planned, "":
				u := unproven(causeNotSent)
				o = [3]Outcome{u, u, u}
			default:
				c := cmp.Or(s.Reason, string(s.Status))
				o = [3]Outcome{failedAs(c, false), failedAs(c, j.inError), failedAs(c, j.inError)}
			}
			out.record(k.discover, string(key), o[0])
			out.record(k.done, string(key), o[1])
			if k.stream == Host {
				out.record(HostEndToEnd, string(key), o[2])
			}
		}
	}
	open[probeapi.UnitTypeDpu] = out.dpus(inv, sent, end, ix, hosts)
	racks := map[string]bool{}
	for _, r := range plan.Racks {
		racks[r.RackID] = true
	}
	out.unplanned(ix, planned, open, racks)
	for _, l := range [][]string{out.ReadErrors, out.Unexpected, out.Pending} {
		slices.Sort(l)
	}
	return out
}

// dpus gives each planned DPU an outcome at host level: a host's DPUs are done as far as its device lists Ready DPUs, failed ones first, and a surplus is never
// done. More DPUs on a planned host than planned is unexpected. It returns how many planned DPUs no planned host lists yet.
func (out *Reconciled) dpus(inv *inventory.SiteInventory, sent map[WriteKey]KeyOutcome, end EndUnits, ix *index, hosts map[WriteKey]string) int {
	planned := map[WriteKey]int{}
	for _, p := range inv.Pods {
		for _, d := range p.Records.DPUs {
			planned[DeviceKey(Host, d.ParentBmcMac)]++
		}
	}
	read := readOK(end.Devices, probeapi.UnitTypeDpu)
	of := map[string][]*probeapi.Unit{}
	for d, u := range ix.devices {
		if d.t == probeapi.UnitTypeDpu && u.Dpu != nil && u.Dpu.HostID != nil {
			of[*u.Dpu.HostID] = append(of[*u.Dpu.HostID], u)
		}
	}
	for key, dev := range hosts {
		if dev != "" && len(of[dev]) > planned[key] {
			out.Unexpected = append(out.Unexpected, fmt.Sprintf("host %s lists %d DPUs, %d planned", key, len(of[dev]), planned[key]))
		}
	}
	owed := 0
	for _, parent := range slices.Sorted(maps.Keys(planned)) {
		n, dev, outs := planned[parent], hosts[parent], []Outcome{}
		if !read || dev == "" {
			owed += n
		} else {
			owed += max(n-len(of[dev]), 0)
		}
		switch st := sent[parent].Status; {
		case (st == Sent || st == Unclear) && dev != "" && !read:
			outs = slices.Repeat([]Outcome{unproven(causeUnread)}, n)
		case (st == Sent || st == Unclear) && dev != "":
			for _, u := range of[dev] {
				outs = append(outs, deviceOutcome(u, false))
			}
			rank := map[string]int{"failed": 0, "done": 1, "unknown": 2}
			slices.SortStableFunc(outs, func(a, b Outcome) int { return rank[a.Outcome] - rank[b.Outcome] })
			outs = outs[:min(len(outs), n)]
			for len(outs) < n {
				outs = append(outs, notYet(causeNotReady))
			}
		case st == Sent || st == Unclear: // the parent has no linked device
			switch p := out.Units[HostDiscovery][string(parent)]; {
			case p.Outcome == "unknown" && p.Unproven:
				outs = slices.Repeat([]Outcome{unproven(p.Cause)}, n)
			case p.Outcome == "unknown": // not linked in the expected sweep, which runs apart from the device sweeps
				outs = slices.Repeat([]Outcome{notYet(causeParentNotLinked)}, n)
			default:
				outs = slices.Repeat([]Outcome{unproven(causeNoRecord)}, n)
			}
		case st == NotSent || st == Planned || st == "":
			outs = slices.Repeat([]Outcome{unproven(causeNotSent)}, n)
		default: // never accepted; the probe puts a host's DPUs in its error stage, so the failed filter counts those its device lists there
			visible := 0
			for _, u := range of[dev] {
				if read && dev != "" && u.Stage == probeapi.StageError {
					visible++
				}
			}
			for i := range n {
				outs = append(outs, failedAs(causeParentNotAccepted, i < visible))
			}
		}
		for i, o := range outs {
			out.record(DPUEndToEnd, fmt.Sprintf("%s#%d", parent, i), o)
		}
	}
	return owed
}

// unplanned reports every device no planned record links, and every DPU no such host lists. A rack whose ID no planned rack has is extra. Per type, the units
// beyond what the open planned keys could still explain are a proven excess, reported as a count; the rest are pending. Duplicates are already reported.
func (out *Reconciled) unplanned(ix *index, planned map[WriteKey]bool, open map[probeapi.UnitType]int, racks map[string]bool) {
	linked := func(d devKey) bool {
		return slices.ContainsFunc(ix.claims[d], func(k WriteKey) bool { return planned[k] })
	}
	unmatched := map[probeapi.UnitType][]string{}
	for d, u := range ix.devices {
		problem := fmt.Sprintf("%s unit %s is not linked by a planned record", d.t, d.id)
		switch {
		case ix.dupDevices[d], d.t != probeapi.UnitTypeDpu && linked(d):
			continue
		case d.t == probeapi.UnitTypeRack && !racks[d.id]:
			out.Unexpected = append(out.Unexpected, problem)
			continue
		case d.t != probeapi.UnitTypeDpu:
		case u.Dpu == nil || u.Dpu.HostID == nil:
			problem = fmt.Sprintf("dpu unit %s is not attached to a host", d.id)
		case !linked(devKey{probeapi.UnitTypeHost, *u.Dpu.HostID}):
			problem = fmt.Sprintf("dpu unit %s is under host %s, which no planned record links", d.id, *u.Dpu.HostID)
		default:
			continue
		}
		unmatched[d.t] = append(unmatched[d.t], problem)
	}
	for t, problems := range unmatched {
		switch room := open[t]; {
		case room == 0:
			out.Unexpected = append(out.Unexpected, problems...)
			continue
		case len(problems) > room:
			out.Unexpected = append(out.Unexpected, fmt.Sprintf("%d %s units the plan does not account for, room for %d: %d extra",
				len(problems), t, room, len(problems)-room))
		}
		out.Pending = append(out.Pending, problems...)
	}
}

// timing is what a stage's summary proves. Without a summary, a timing error or a whole set of crossings the times are unknown: unresolved, never read as zero.
// A crossing not reached by the window's end is a deadline passed once the final scrape was seen; before it, the sweep that reached it may not be stored yet.
func timing(s *probeapi.SummaryResponse) result.StageStatus {
	switch {
	case s == nil || s.TimingErrorSeconds == nil || s.MaxSweepGapSeconds == nil || len(s.Crossings) != len(probeapi.FixedQuantiles):
		return result.StageUnresolved
	case !slices.ContainsFunc(s.Crossings, func(c probeapi.Crossing) bool { return c.Seconds == nil }):
		return result.StageDone
	case s.FinalScrapeSeen:
		return result.StageDeadlinePassed
	}
	return result.StageUnresolved
}

// stageResult is a stage's admitted result. A zero-N stage is not applicable. Else the units are the planned cohort's, N stays planned, and the status is the
// more severe of the timing's and the cohort's: any failed unit fails it and any unknown unit or count gap leaves it unresolved, so per-key evidence never lifts
// a timing that is not done.
func stageResult(st Stage, t *Tally, timing result.StageStatus) result.StageResult {
	if st.N == 0 {
		return result.StageResult{ID: st.ID, Status: result.StageNotApplicable, Units: result.UnitCountsOf(0, 0, 0)}
	}
	t = cmp.Or(t, &Tally{})
	failed, cohort := t.Failed+t.FailedOutsideFilter, result.StageDone
	switch {
	case failed > 0:
		cohort = result.StageFailed
	case t.Unknown > 0 || t.Done+failed+t.Unknown != st.N:
		cohort = result.StageUnresolved
	}
	order := []result.StageStatus{result.StageDone, result.StageUnresolved, result.StageDeadlinePassed, result.StageFailed}
	rank := func(s result.StageStatus) int {
		if i := slices.Index(order, s); i >= 0 {
			return i
		}
		return 1 // a status outside the order is unresolved
	}
	return result.StageResult{ID: st.ID, Status: order[max(rank(timing), rank(cohort))], Units: result.UnitCountsOf(st.N, t.Done, failed)}
}
