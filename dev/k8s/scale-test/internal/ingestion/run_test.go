// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"os"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"google.golang.org/grpc/codes"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/clustertest"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/store"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// fakeProbe serves units in one page per read and counts the units a filter selects, plus extra per type, after hold. Every reply's sweep is age s old
// (stale over 20).
type fakeProbe struct {
	mu    sync.Mutex
	units []probeapi.Unit
	extra map[probeapi.UnitType]uint64
	age   float64
	hold  func(f probeapi.Filter)
}

func (p *fakeProbe) set(f func(p *fakeProbe)) { p.mu.Lock(); defer p.mu.Unlock(); f(p) }

func (p *fakeProbe) selected(f probeapi.Filter) []probeapi.Unit {
	p.mu.Lock()
	defer p.mu.Unlock()
	return slices.DeleteFunc(slices.Clone(p.units), func(u probeapi.Unit) bool {
		e, h := u.Expected, u.Host
		return u.Type != f.Type || f.Stage != nil && u.Stage != *f.Stage || f.RecordType != nil && e.RecordType != *f.RecordType ||
			f.Linked != nil && e.Linked != *f.Linked || f.Allocatable != nil && (h.Allocatable == nil || *h.Allocatable != *f.Allocatable)
	})
}

func (p *fakeProbe) sweep() probeapi.SweepInfo {
	p.mu.Lock()
	defer p.mu.Unlock()
	return probeapi.SweepInfo{SnapshotID: "s1", Family: probeapi.FamilyHost, SweepAgeSeconds: p.age, SweepIntervalSeconds: 10}
}

func (p *fakeProbe) Count(_ context.Context, f probeapi.Filter) (probeapi.CountResponse, error) {
	if p.hold != nil {
		p.hold(f)
	}
	return probeapi.CountResponse{Count: uint64(len(p.selected(f))) + p.extra[f.Type], SweepInfo: p.sweep()}, nil
}
func (p *fakeProbe) Units(_ context.Context, f probeapi.Filter, _ *string, _ probe.Page) (probeapi.UnitsResponse, error) {
	return probeapi.UnitsResponse{SweepInfo: p.sweep(), Units: p.selected(f)}, nil
}
func (p *fakeProbe) UnitsQuery(context.Context, probeapi.UnitsQuery) (probeapi.UnitsResponse, error) {
	return probeapi.UnitsResponse{}, errors.New("unused")
}
func (p *fakeProbe) Config(context.Context, probe.ConfigFilter) (probeapi.ConfigResponse, error) {
	return probeapi.ConfigResponse{}, errors.New("unused")
}

// fakeStore answers each summary with reply, done by default, and keeps what it was asked by filter.
type fakeStore struct {
	mu    sync.Mutex
	asked map[string]probeapi.SummaryParams
	reply func(ctx context.Context, f probeapi.Filter, p probeapi.SummaryParams) (probeapi.SummaryResponse, error)
}

func (*fakeStore) Preflight(context.Context, time.Time, ...probeapi.Family) error { return nil }
func (*fakeStore) FamilyStatus(context.Context, time.Time, probeapi.Family) (store.Status, error) {
	return store.Status{}, nil
}
func (s *fakeStore) Summarize(ctx context.Context, _ time.Time, f probeapi.Filter, p probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
	s.mu.Lock()
	s.asked[fkey(f)] = p
	s.mu.Unlock()
	if s.reply != nil {
		return s.reply(ctx, f, p)
	}
	return *summary(new(1.0)), nil
}

func fkey(f probeapi.Filter) string { q, _ := f.Query(); return q.Encode() }

type fakePods struct {
	mu   sync.Mutex
	pods map[string][]cluster.PodInfo // by namespace
	fail error
}

// set gives the first pod of namespace ns the UID uid: the pod recreated under its name.
func (p *fakePods) set(ns, uid string) { p.mu.Lock(); defer p.mu.Unlock(); p.pods[ns][0].UID = uid }

func (p *fakePods) ListPods(_ context.Context, ns, _ string) ([]cluster.PodInfo, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return slices.Clone(p.pods[ns]), p.fail
}

// runRig is an ingestion of site() on a ready site whose state is already the clean end state, with real writes through the journaled writer to fakeForge.
type runRig struct {
	*rig
	probe *fakeProbe
	store *fakeStore
	pods  *fakePods
	cfg   *config.Config
	test  *Test
	ready []readiness.Check
	until time.Duration // the deadline after the start, an hour when zero
	inv   *inventory.SiteInventory
}

func newRunRig(t *testing.T) *runRig {
	r := &runRig{rig: newRig(t), probe: &fakeProbe{units: ingested(site()), age: 1}, store: &fakeStore{asked: map[string]probeapi.SummaryParams{}},
		pods:  &fakePods{pods: map[string][]cluster.PodInfo{"mat": {{Name: "mat-0"}}, "nico": {{Name: "api-0", Restarts: 1}}}},
		ready: []readiness.Check{{Name: "probe", OK: true}}}
	r.cfg = &config.Config{Namespace: "mat", Probe: config.Probe{PollSeconds: 10}, Timeouts: config.Timeouts{ReadinessSeconds: 60}, ResultMaxBytes: 900_000,
		ControlPlane: []config.PodSelector{{Namespace: "nico", LabelSelector: "app=nico-api"}}}
	s, err := ParseSettings(yaml.Node{})
	if err != nil {
		t.Fatal(err)
	}
	d := readiness.Deps{Pods: r.pods, Store: func(context.Context) (readiness.StoreProbe, error) { return readiness.StoreProbe{Client: r.store}, nil }}
	r.test = New(r.cfg, s, d, r.probe, func(string) (ForgeWriter, error) { return r.f, nil })
	r.test.check = func(context.Context, readiness.Deps, time.Time) readiness.Report {
		return readiness.Report{Checks: r.ready, Restarts: map[string]uint32{"nico/api-0": 1}, Inventory: *cmp.Or(r.inv, site())}
	}
	return r
}

// run runs the test with the rig's deadline and the probe read end readEnd away, and decodes its detail.
func (r *runRig) run(t *testing.T, ctx context.Context, dryRun bool, readEnd time.Duration) (result.Outcome, detail, error) {
	t.Helper()
	now := time.Now()
	rc := run.RunCtx{Ctx: ctx, Clock: run.RealClock{}, Deadline: now.Add(cmp.Or(r.until, time.Hour)), ProbeReadEnd: now.Add(readEnd), DryRun: dryRun}
	out, err := r.test.Run(rc, r.w)
	r.w.Close()
	var d map[string]detail
	if out.Detail != nil {
		if err := json.Unmarshal(out.Detail, &d); err != nil {
			t.Fatal(err)
		}
	}
	return out, d["ingestion"], err
}

func statusesOf(out result.Outcome) map[result.StageID]result.StageStatus {
	got := map[result.StageID]result.StageStatus{}
	for _, s := range out.Stages {
		got[s.ID] = s.Status
	}
	return got
}

func every(status result.StageStatus, except map[result.StageID]result.StageStatus) map[result.StageID]result.StageStatus {
	out := map[result.StageID]result.StageStatus{}
	for _, s := range Stages(site().N) {
		out[s.ID] = status
		if e, ok := except[s.ID]; ok {
			out[s.ID] = e
		}
	}
	return out
}

// TestRunVerdicts checks stream T0s, unknown/deadline-passed stages, live instance and pod invariants, and reasons for every non-pass.
func TestRunVerdicts(t *testing.T) {
	for _, tc := range []struct {
		name    string
		setup   func(r *runRig)
		verdict result.Verdict
		stages  map[result.StageID]result.StageStatus
	}{
		{"clean", func(r *runRig) { r.f.during = func(context.Context, string) { time.Sleep(time.Second) } }, result.Passed, every(result.StageDone, nil)},
		{"a Ready instance created during dispatch", func(r *runRig) {
			u := probeapi.Unit{ID: "instance", Type: probeapi.UnitTypeInstance, Stage: probeapi.StageReady}
			r.f.during = func(context.Context, string) { r.probe.set(func(p *fakeProbe) { p.units = append(p.units, u) }) }
		}, result.Failed, every(result.StageDone, nil)},
		{"a Error instance created during dispatch", func(r *runRig) {
			u := probeapi.Unit{ID: "instance", Type: probeapi.UnitTypeInstance, Stage: probeapi.StageError}
			r.f.during = func(context.Context, string) { r.probe.set(func(p *fakeProbe) { p.units = append(p.units, u) }) }
		}, result.Failed, every(result.StageDone, nil)},
		{"a host never Ready", func(r *runRig) {
			r.probe.set(func(p *fakeProbe) { find(p.units, "dev-AA:00").Stage = probeapi.StageRegistered })
			r.store.reply = func(_ context.Context, f probeapi.Filter, _ probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
				if f.Type == probeapi.UnitTypeHost {
					return *summary(new(1.0), nil), nil
				}
				return *summary(new(1.0)), nil
			}
		}, result.Failed, every(result.StageDone, map[result.StageID]result.StageStatus{HostEndToEnd: result.StageDeadlinePassed,
			HostReadyDiagnostic: result.StageDeadlinePassed})},
		{"a summary the store cannot give", func(r *runRig) {
			r.store.reply = func(_ context.Context, f probeapi.Filter, _ probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
				if f.Type == probeapi.UnitTypeDpu {
					return probeapi.SummaryResponse{}, history.Unavailable{Text: "a sweep in the window is from another probe process"}
				}
				return *summary(new(1.0)), nil
			}
		}, result.Unresolved, every(result.StageDone, map[result.StageID]result.StageStatus{DPUEndToEnd: result.StageUnresolved})},
		{"stale end reads", func(r *runRig) {
			r.f.during = func(context.Context, string) { r.probe.set(func(p *fakeProbe) { p.age = 41 }) }
		}, result.Unresolved, every(result.StageUnresolved, nil)},
		{"an unclear write with no record", func(r *runRig) {
			r.f.reply = func(call string, _ int) reply {
				if call == "machine AA:00" {
					return st(codes.Unavailable)
				}
				return reply{}
			}
			r.probe.set(func(p *fakeProbe) {
				p.units = slices.DeleteFunc(p.units, func(u probeapi.Unit) bool { return u.ID == "rec-AA:00" })
			})
		}, result.Unresolved, every(result.StageDone, map[result.StageID]result.StageStatus{HostDiscovery: result.StageUnresolved,
			HostEndToEnd: result.StageUnresolved, HostReadyDiagnostic: result.StageUnresolved, DPUEndToEnd: result.StageUnresolved})},
		{"a control-plane pod restarted", func(r *runRig) {
			r.f.during = func(context.Context, string) {
				r.pods.mu.Lock()
				r.pods.pods["nico"][0].Restarts = 2
				r.pods.mu.Unlock()
			}
		}, result.Failed, every(result.StageDone, nil)},
		{"a MAT pod replaced", func(r *runRig) { r.f.during = func(context.Context, string) { r.pods.set("mat", "u2") } }, result.Failed, every(result.StageDone, nil)},
		{"a control-plane pod replaced", func(r *runRig) { r.f.during = func(context.Context, string) { r.pods.set("nico", "u2") } }, result.Failed,
			every(result.StageDone, nil)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRunRig(t)
				tc.setup(r)
				start := time.Now()
				out, d, err := r.run(t, context.Background(), false, 2*time.Hour)
				is(t, []any{err, out.Verdict, statusesOf(out)}, []any{nil, tc.verdict, tc.stages}, "error, verdict and stages")
				is(t, []any{len(out.Invariants), len(out.Reasons) > 0}, []any{6, tc.verdict != result.Passed}, "every invariant has a result, a reason unless passed")
				if strings.Contains(tc.name, "instance created") {
					is(t, []any{out.Invariants[2].Name, out.Invariants[2].Held}, []any{"nicoInstances", false}, "every instance stage breaks the zero-instance invariant")
				}
				if tc.name == "a host never Ready" {
					is(t, out.Reasons, []string{"stage host.endToEnd: deadlinePassed 1/2", "stage host.readyDiagnostic: deadlinePassed 1/2"}, "the late stages")
				}
				if tc.name != "clean" {
					return
				}
				host, dpu, rack := r.store.asked[fkey(hostGate())], r.store.asked[fkey(Stages(site().N)[5].Filter)], r.store.asked[fkey(Stages(site().N)[1].Filter)]
				is(t, []any{host.Start == dpu.Start, host.Start.Before(dpu.End.Time), rack.Start.Sub(start), host.N, len(r.store.asked)},
					[]any{true, true, time.Second, uint64(2), 10}, "a DPU stage is timed from the host stream's T0, a rack stage from its first rack, not group, send")
				is(t, []any{len(d.Outcomes), d.Sends[Host][Sent], d.Stages[3].T0.Equal(host.Start.Time)}, []any{0, 2, true}, "the detail")
			})
		})
	}
}

// blockedProbe holds every count until its context ends and notes the deadline that context carried.
type blockedProbe struct {
	*fakeProbe
	deadline *time.Time
}

func (p blockedProbe) Count(ctx context.Context, _ probeapi.Filter) (probeapi.CountResponse, error) {
	*p.deadline, _ = ctx.Deadline()
	<-ctx.Done()
	return probeapi.CountResponse{}, ctx.Err()
}

// TestTheObserversReadsEndAtTheWorkDeadline: a polling read waits on a context that carries the work deadline, not the later probe read end, and the run's cancel.
func TestTheObserversReadsEndAtTheWorkDeadline(t *testing.T) {
	for _, tc := range []struct {
		name           string
		cancelAt, want time.Duration // after the start; none when zero
	}{{"deadline", 0, time.Minute}, {"cancel", 10 * time.Second, 10 * time.Second}} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				if tc.cancelAt > 0 {
					time.AfterFunc(tc.cancelAt, cancel)
				}
				var got time.Time
				start := time.Now()
				o := observe(ctx, blockedProbe{&fakeProbe{}, &got}, Stages(site().N), countInvariants(), time.Second, start.Add(time.Minute), start.Add(time.Hour))
				o.stop(start.Add(time.Hour))
				is(t, []time.Duration{got.Sub(start), time.Since(start)}, []time.Duration{time.Minute, tc.want}, "the read's deadline, the observer's end")
			})
		})
	}
}

// TestALateCrossingIsNotTheRuns: a host count read before the deadline that answers after it, with every host Ready by then, is not taken, and every
// summary ends at the deadline, so a store that counts only its window finds the hosts not Ready in time: those stages passed their deadline.
func TestALateCrossingIsNotTheRuns(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r, deadline, readyAt := newRunRig(t), time.Now().Add(time.Hour), time.Time{}
		r.probe.set(func(p *fakeProbe) { find(p.units, "dev-AA:00").Stage = probeapi.StageRegistered })
		r.probe.hold = func(f probeapi.Filter) {
			if fkey(f) == fkey(hostGate()) && readyAt.IsZero() && time.Until(deadline) <= 10*time.Second {
				time.Sleep(11 * time.Second)
				r.probe.set(func(p *fakeProbe) { find(p.units, "dev-AA:00").Stage = probeapi.StageReady })
				readyAt = time.Now()
			}
		}
		r.store.reply = func(_ context.Context, f probeapi.Filter, p probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
			if f.Type == probeapi.UnitTypeHost && p.End.Before(readyAt) {
				return *summary(new(1.0), nil), nil
			}
			return *summary(new(1.0)), nil
		}
		out, d, err := r.run(t, context.Background(), false, 2*time.Hour)
		late := slices.ContainsFunc(slices.Collect(maps.Values(r.store.asked)), func(p probeapi.SummaryParams) bool { return p.End.After(deadline) })
		is(t, []any{err, late, out.Verdict, d.Observed.Count[HostEndToEnd]}, []any{nil, false, result.Failed, uint64(1)}, "error, a summary past the deadline, count")
		is(t, statusesOf(out), every(result.StageDone, map[result.StageID]result.StageStatus{HostEndToEnd: result.StageDeadlinePassed,
			HostReadyDiagnostic: result.StageDeadlinePassed}), "stages")
	})
}

// TestRunSendsNothingUnlessReady: a failed readiness check, a stale invariant baseline (never read as 0), an instance or mismatch already present, a failed
// MAT or control-plane pod capture, no store, or readiness that answers past the deadline (which bounds it) is notReady with its reason, and nothing is
// journaled or sent.
func TestRunSendsNothingUnlessReady(t *testing.T) {
	extra := func(t probeapi.UnitType) map[probeapi.UnitType]uint64 { return map[probeapi.UnitType]uint64{t: 1} }
	for _, tc := range []struct {
		name, reason string
		setup        func(r *runRig)
	}{
		{"readiness", "bmc: 1 of 1 BMCs failed", func(r *runRig) { r.ready = []readiness.Check{{Name: "bmc", Detail: "1 of 1 BMCs failed"}} }},
		{"a write plan", "writePlan: a DPU device has no expected record (pod 0, device 0)", func(r *runRig) {
			r.test.check = func(context.Context, readiness.Deps, time.Time) readiness.Report {
				return readiness.Report{Inventory: *inv(nil, inventory.Device{Kind: inventory.KindDPU, BmcMac: secret})}
			}
		}},
		{"a stale baseline", "invariantBaseline: ambiguousLinks: the host sweep is 41 s old", func(r *runRig) { r.probe.age = 41 }},
		{"an instance", "invariantBaseline: nicoInstances: 1 present, 0 required on a greenfield site", func(r *runRig) { r.probe.extra = extra(probeapi.UnitTypeInstance) }},
		{"an ambiguous link", "invariantBaseline: ambiguousLinks: 1 present", func(r *runRig) { r.probe.extra = extra(probeapi.UnitTypeIdentityMismatch) }},
		{"a device without record", "invariantBaseline: devicesWithoutRecord: 1 present", func(r *runRig) { r.probe.extra = extra(probeapi.UnitTypeIdentityMismatch) }},
		{"MAT pods", "matPods: listing app.kubernetes.io/component=machine-a-tron in mat: lost", func(r *runRig) { r.pods.fail = errors.New("lost") }},
		{"control-plane pods", "controlPlanePods: listing app=nico-api in nico: lost", func(r *runRig) { r.pods.fail = errors.New("lost") }},
		{"no store", "metricsStore: the store is not checked", func(r *runRig) { r.test.deps.Store = nil }},
		{"a large plan at the least budget", "resultSize: the largest result of this plan takes", func(r *runRig) { r.inv, r.cfg.ResultMaxBytes = largeFleet(250, true), 262_144 }},
		{"readiness past the deadline", "workDeadline: the run deadline passed before the first write", func(r *runRig) {
			check := r.test.check
			r.until = 100 * time.Millisecond
			r.test.check = func(ctx context.Context, d readiness.Deps, until time.Time) readiness.Report {
				rep := check(ctx, d, until)
				if time.Until(until) > r.until {
					rep.Checks = []readiness.Check{{Name: "readiness", Detail: "bounded past the deadline"}}
				}
				time.Sleep(2 * r.until)
				return rep
			}
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := newRunRig(t)
			tc.setup(r)
			out, _, err := r.run(t, context.Background(), false, 2*time.Hour)
			data, _ := os.ReadFile(r.j.Path())
			is(t, []any{err, out.Verdict, len(r.f.seen()), strings.Count(string(data), `"intent"`)}, []any{nil, result.NotReady, 0, 0}, "verdict, calls, intents")
			if !slices.ContainsFunc(out.Reasons, func(s string) bool { return strings.HasPrefix(s, tc.reason) }) {
				t.Errorf("reasons %q, want %q", out.Reasons, tc.reason)
			}
		})
	}
}

// largeFleet has 18 hosts of 2 DPUs, 9 switches and 8 power shelves per rack, with short or UUID rack IDs.
func largeFleet(racks int, uuid bool) *inventory.SiteInventory {
	n := uint64(racks)
	big := &inventory.SiteInventory{Pods: []inventory.MatPod{{}}, N: inventory.TypeCounts{RackGroup: n, Rack: n, Host: 18 * n, DPU: 36 * n, Switch: 9 * n, PowerShelf: 8 * n}}
	rec, mac := &big.Pods[0].Records, func(kind string, n int) string {
		return fmt.Sprintf("%s:00:00:%02x:%02x:%02x", kind, n>>16, n>>8&0xff, n&0xff)
	}
	for i := range racks {
		id := fmt.Sprintf("rack-%03d", i+1)
		if uuid {
			id = fmt.Sprintf("%08x-0000-4000-8000-%012x", i, i)
		}
		rec.Racks = append(rec.Racks, rackIntent(id))
		for j := range 18 {
			n := i*18 + j
			rec.Devices = append(rec.Devices, inventory.Device{Kind: inventory.KindMachine, BmcMac: mac("aa", n), Serial: fmt.Sprintf("h%05d", n), RackID: &id})
			rec.DPUs = append(rec.DPUs, inventory.DPU{BmcMac: mac("dd", 2*n), ParentBmcMac: mac("aa", n)}, inventory.DPU{BmcMac: mac("dd", 2*n+1), ParentBmcMac: mac("aa", n)})
		}
		for j := range 9 {
			rec.Devices = append(rec.Devices, inventory.Device{Kind: inventory.KindSwitch, BmcMac: mac("bb", i*9+j), RackID: &id})
		}
		for j := range 8 {
			rec.Devices = append(rec.Devices, inventory.Device{Kind: inventory.KindPowerShelf, BmcMac: mac("cc", i*8+j), RackID: &id})
		}
	}
	return big
}

// TestFleetPlansFitTheResult keeps the 250-rack budget, admits 556 and 1,100 racks at the default, and leaves room for UUID IDs.
// Every unit unknown and every write unclear, or every unit failed, fits the bound with every key kept and a small ConfigMap status.
func TestFleetPlansFitTheResult(t *testing.T) {
	cfg, err := config.Parse([]byte("runId: run\ncommand: ingestion\ndryRun: false\nnamespace: mat\ntimeouts: {wholeRunSeconds: 3600, finalizeSeconds: 1800}\nsettings: {}\n"), "ingestion")
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		racks  int
		uuid   bool
		budget uint32
	}{{250, true, 1_000_000}, {556, false, cfg.ResultMaxBytes}, {1100, false, cfg.ResultMaxBytes}, {1100, true, cfg.ResultMaxBytes}} {
		t.Run(fmt.Sprintf("%d/uuid=%t", tc.racks, tc.uuid), func(t *testing.T) { checkResultBudget(t, tc.racks, tc.uuid, tc.budget) })
	}
}

func checkResultBudget(t *testing.T, racks int, uuid bool, budget uint32) {
	t.Helper()
	big, r := largeFleet(racks, uuid), newRunRig(t)
	r.probe.units, r.cfg.ResultMaxBytes = nil, budget
	for o, uncertain := range map[KeyOutcome]int{{Status: Unclear}: 37 * racks, {Status: Rejected, Reason: "pushback-grpc-ResourceExhausted"}: 0} {
		now := time.Now()
		in := &ingest{t: r.test, rc: run.RunCtx{Ctx: context.Background(), Clock: run.RealClock{}, Deadline: now.Add(time.Hour), ProbeReadEnd: now.Add(2 * time.Hour)},
			inv: big, stages: Stages(big.N), invs: countInvariants(), seen: &observed{}}
		is(t, []any{in.gate(now.Add(time.Minute), now.Add(time.Hour), true), in.d.Checks}, []any{true, []readiness.Check(nil)}, "admitted at the result budget")
		worst := in.worstSize()
		if o.Status == Unclear {
			r.cfg.ResultMaxBytes = uint32(worst - 1)
			is(t, in.gate(now.Add(time.Minute), now.Add(time.Hour), true), false, "a smaller result budget refuses the plan")
			is(t, in.d.Checks, []readiness.Check{{Name: "resultSize", Detail: fmt.Sprintf("the largest result of this plan takes %d bytes, over resultMaxBytes %d", worst, worst-1)}}, "refusal names the needed bytes")
			in.d.Checks, r.cfg.ResultMaxBytes = nil, budget
		}
		in.res = Results{Outcomes: map[WriteKey]KeyOutcome{}}
		for _, k := range in.plan.keys() {
			in.res.Outcomes[k] = o
		}
		out, at, n, unclear := in.end(true), jsonx.Time{Time: now}, uint64(37*racks), uint64(uncertain)
		full := result.RunResult{SchemaVersion: 1, Test: r.test.Name(), RunID: "run", ExecutionID: "exec", DryRun: new(false), Outcome: out, Reasons: out.Reasons,
			Clock: result.RunClock{StartedAt: at, Deadline: &at, EndedAt: at, ClockSyncAssumption: result.ClockSyncAssumption}, Lock: result.LockResult{Name: "lock", Kind: "ingestion", AcquiredAt: &at, FinalState: new("released")},
			Stages: out.Stages, Invariants: out.Invariants, Journal: result.JournalSummary{Ops: &n, Attempts: &n, Unclear: &unclear}, Result: result.ResultRef{Claim: "claim", Path: "/journal/result.json"}, Detail: out.Detail}
		data, err := jsonx.Encode(&full)
		status, serr := jsonx.Encode(result.StatusOf(&full))
		var d map[string]detail
		_ = json.Unmarshal(out.Detail, &d)
		var listed []WriteKey
		for _, keys := range d["ingestion"].Outcomes {
			listed = append(listed, keys...)
		}
		want := in.plan.UnitKeys()
		for _, k := range in.plan.UnitKeys() {
			if strings.HasPrefix(string(k), "host/") {
				want = append(want, k+"#0", k+"#1")
			}
		}
		slices.Sort(listed)
		slices.Sort(want)
		sameUnclear := uncertain == 0 || slices.Equal(slices.Sorted(slices.Values(d["ingestion"].Uncertain)), slices.Sorted(slices.Values(in.plan.keys())))
		is(t, []any{err, serr, len(data) <= worst, worst <= int(budget), racks == 250 || 2*worst <= int(budget), slices.Equal(listed, want), len(listed), len(d["ingestion"].Uncertain), sameUnclear, len(status) < 16<<10},
			[]any{nil, nil, true, true, true, true, 72 * racks, uncertain, true, true}, string(o.Status))
		t.Logf("%d racks, uuid=%t, %s: result=%d, worstSize=%d, budget=%d, status=%d bytes", racks, uuid, o.Status, len(data), worst, budget, len(status))
	}
}

// Every write may use all 30 attempts. Bound the journal using the actual intent/receipt encoding, longest counters and time, and a whole line per in-flight receipt.
func TestJournalFitsTheFleet(t *testing.T) {
	_, recs, err := Records(largeFleet(1100, true))
	if err != nil {
		t.Fatal(err)
	}
	encode := func(v any) uint64 {
		b, e := jsonx.Encode(v)
		if e != nil {
			t.Fatal(e)
		}
		return uint64(len(b) + 1)
	}
	n, at := uint64(len(recs)), jsonx.Time{Time: time.Date(9999, 12, 31, 23, 59, 59, 999999999, time.UTC)}
	receipt := encode(journal.Receipt{Entry: "receipt", Seq: n * 30, Op: n, At: at,
		Outcome: journal.Outcome{Kind: journal.Rejected, Reason: journal.Pushback(string(journal.Grpc("ResourceExhausted")))}})
	c := DefaultConcurrency
	bytes := uint64(1+c.Rack+c.Host+c.Switch+c.PowerShelf) * journal.MaxLineBytes
	for k, r := range recs {
		bytes += 30 * (receipt + encode(journal.Intent{Entry: "intent", Seq: n * 30, Op: n, Attempt: 30, Key: string(k), Kind: r.Kind,
			Idempotent: true, Hosts: []string{}, RequestDigest: r.Digest, At: at}))
	}
	is(t, 2*bytes <= journal.MaxJournalBytes, true, "all writes and retries have at least 2x journal capacity")
	t.Logf("1100 racks, %d writes, 30 attempts: journal bound=%d, cap=%d bytes", n, bytes, journal.MaxJournalBytes)
}

// TestADPUsOutcomeIsNotFoldedIntoItsHost: a Ready host's failed DPU #0 and unknown DPU #1 keep their own keys and causes.
func TestADPUsOutcomeIsNotFoldedIntoItsHost(t *testing.T) {
	stages, rec := []Stage{{ID: HostEndToEnd}, {ID: DPUEndToEnd}}, &Reconciled{Units: map[result.StageID]map[string]Outcome{
		HostEndToEnd: {"host/a": {Outcome: "done"}}, DPUEndToEnd: {"host/a#0": {Outcome: "failed", Cause: "error"}, "host/a#1": {Outcome: "unknown", Cause: "notYet"}}}}
	is(t, outcomes(stages, rec), map[string][]WriteKey{"failed:error": {"host/a#0"}, "unknown:notYet": {"host/a#1"}}, "outcomes")
}

// oneCM is the result ConfigMap in memory, for a real Execute, which calls it from one goroutine.
type oneCM struct{ o *cluster.ConfigMapObject }

func (c *oneCM) GetConfigMap(context.Context, string) (*cluster.ConfigMapObject, error) {
	return c.o, nil
}
func (c *oneCM) CreateConfigMap(ctx context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	return c.ReplaceConfigMap(ctx, o)
}
func (c *oneCM) ReplaceConfigMap(_ context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	o.UID, c.o = "u1", &o
	return o, nil
}

// TestAnUnclearSendIsKept: under a real Execute, a host or rack group write whose committed reply was lost (its retry finds the record) stays in the persisted
// result even when the end shows it Ready and every crossing came: the run is never a clean pass and releases the site needsCleanup.
func TestAnUnclearSendIsKept(t *testing.T) {
	for _, tc := range []struct {
		call    string
		key     WriteKey
		verdict result.Verdict
	}{{"machine AA:00", "host/aa:00", result.Unresolved}, {"group g-r2", "rackGroup/r2", result.Failed}} {
		synctest.Test(t, func(t *testing.T) {
			r := newRunRig(t)
			r.f.reply = func(call string, n int) reply {
				switch {
				case call != tc.call:
					return reply{}
				case n == 1:
					return st(codes.Unavailable)
				}
				return st(codes.AlreadyExists)
			}
			r.cfg.RunID, r.cfg.ExecutionID, r.cfg.DryRun, r.cfg.Site.LockName, r.cfg.ConfigMaps.Result = "r1", "r1-pod", new(false), "lock", "r1-result"
			r.cfg.Timeouts, r.cfg.Journal = config.Timeouts{WholeRunSeconds: 7200, ReadinessSeconds: 60, FinalizeSeconds: 1800}, config.Journal{Dir: t.TempDir(), Claim: "c"}
			res := run.Execute(run.Deps{Config: r.cfg, Lock: &lock.SiteLock{Cluster: clustertest.New("mat"), Name: "lock", Timing: lock.DefaultTiming}, CM: &oneCM{}}, r.test)
			data, _ := os.ReadFile(res.Result.Path)
			var got struct {
				Verdict result.Verdict
				Detail  map[string]detail
			}
			_ = json.Unmarshal(data, &got)
			is(t, []any{got.Verdict, got.Detail["ingestion"].Uncertain, res.Lock.FinalState}, []any{tc.verdict, []WriteKey{tc.key}, new(string(lock.NeedsCleanup))}, tc.call)
		})
	}
}

// TestDryRunGatesJournalsAndSendsNothing: a dry run still needs a ready site, then journals every key, sends none and is go.
func TestDryRunGatesJournalsAndSendsNothing(t *testing.T) {
	for _, ready := range []bool{true, false} {
		r := newRunRig(t)
		if !ready {
			r.ready = []readiness.Check{{Name: "bmc"}}
		}
		out, d, err := r.run(t, context.Background(), true, 2*time.Hour)
		data, _ := os.ReadFile(r.j.Path())
		want := []any{nil, result.Go, 0, 8, 2}
		if !ready {
			want = []any{nil, result.NotReady, 0, 0, 0}
		}
		is(t, []any{err, out.Verdict, len(r.f.seen()), strings.Count(string(data), `"intent"`), d.Sends[Host][Planned]}, want, "dry run")
	}
}

// TestAnInterruptedDryRunIsNotGo: a dry run, racks one at a time, whose third intent (the second rack's group) is held past the deadline plans only the first rack
// and its group: nothing is sent, the journal shows exactly those two keys, and the run is not go.
func TestAnInterruptedDryRunIsNotGo(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r, intents := newRunRig(t), atomic.Int32{}
		r.until, r.test.s.Writes.Concurrency.Rack = 10*time.Second, 1
		r.j.AfterSync = func(e any) {
			if _, ok := e.(journal.Intent); ok && intents.Add(1) == 3 {
				time.Sleep(15 * time.Second)
			}
		}
		out, d, err := r.run(t, context.Background(), true, 2*time.Hour)
		data, _ := os.ReadFile(r.j.Path())
		is(t, []any{err, out.Verdict, len(r.f.seen()), d.Sends["rackGroup"], d.Sends[Rack], d.Sends[Host][Planned] + d.Sends[Switch][Planned] + d.Sends[PowerShelf][Planned],
			strings.Count(string(data), `"kind":"planned"`), strings.Count(string(data), `"kind":"notDispatched"`)}, []any{nil, result.NotReady, 0, map[Status]int{Planned: 1, NotSent: 1},
			map[Status]int{Planned: 1, NotSent: 1}, 0, 2, 1}, "error, verdict, RPCs, groups, racks, devices planned, planned and notDispatched receipts")
	})
}

// TestAStopKeepsEveryOutcome: a lock loss during the sends ends them with the cause, keeps every key's outcome and what the end reads allow, summarizes no
// stage and passes none.
func TestAStopKeepsEveryOutcome(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRunRig(t)
		r.f.during = func(context.Context, string) { r.gate.lost.Store(true) }
		out, d, err := r.run(t, context.Background(), false, 2*time.Hour)
		total := 0
		for _, n := range d.Sends {
			for _, c := range n {
				total += c
			}
		}
		is(t, []any{errors.Is(err, writer.ErrLockLost), total, len(out.Invariants), statusesOf(out)},
			[]any{true, 8, 6, every(result.StageUnresolved, nil)}, "cause, outcomes, invariants and stages")
		is(t, len(d.Stages) == 10 && d.Stages[0].Unavailable == "not summarized: the sends ended on an error", true, "no stage summarized")
	})
}

// TestEndReadsAreGuarded: a cancel during the end reads, or the probe read end, cuts the read in flight and refuses every later one with fixed text; no stage
// they cut passes.
func TestEndReadsAreGuarded(t *testing.T) {
	for _, tc := range []struct {
		name    string
		cancel  bool
		readEnd time.Duration
		want    string
	}{
		{"cancel", true, 2 * time.Hour, errReadsCancelled.Error()},
		{"read end", false, 2 * time.Minute, errPastReadEnd.Error()},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRunRig(t)
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				r.store.reply = func(ctx context.Context, _ probeapi.Filter, _ probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
					if tc.cancel {
						time.AfterFunc(time.Second, cancel)
					}
					<-ctx.Done()
					return probeapi.SummaryResponse{}, history.Timeout{}
				}
				out, d, err := r.run(t, ctx, false, tc.readEnd)
				is(t, []any{out.Verdict, len(d.StageReadErrors), statusesOf(out), tc.cancel == (err != nil)},
					[]any{result.Unresolved, 10, every(result.StageUnresolved, nil), true}, "verdict, cut reads, stages, error")
				for _, e := range d.StageReadErrors {
					if !strings.HasSuffix(e, " summary: "+tc.want) {
						t.Errorf("stage read error %q, want %q", e, tc.want)
					}
				}
				is(t, len(r.store.asked), 1, "no summary starts after the cut")
			})
		})
	}
}

// TestNoUpstreamTextReachesTheResult: device reasons and nico-api status messages, which may hold anything, never reach the reasons, detail or invariants. The
// rejected switch stream has a T0 and no end, so it is not summarized.
func TestNoUpstreamTextReachesTheResult(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRunRig(t)
		r.f.reply = func(call string, _ int) reply {
			if strings.HasPrefix(call, "switch") {
				return st(codes.InvalidArgument)
			}
			return reply{}
		}
		r.probe.set(func(p *fakeProbe) {
			for i := range p.units {
				p.units[i].Stage, p.units[i].Reason = probeapi.StageError, new(secret)
			}
		})
		out, d, _ := r.run(t, context.Background(), false, 2*time.Hour)
		is(t, []any{d.Stages[6].T0 != nil, d.Stages[6].WindowEnd, d.Stages[6].Unavailable}, []any{true, (*jsonx.Time)(nil), "no usable window end: no accepted or unclear write"}, "switch")
		all, _ := json.Marshal([]any{out.Reasons, out.Invariants, out.Detail})
		if out.Verdict != result.Failed || strings.Contains(string(all), "SECRET") || strings.Contains(string(all), "pw@") {
			t.Errorf("verdict %s; the result holds upstream text: %s", out.Verdict, all)
		}
	})
}

func TestParseSettings(t *testing.T) {
	parse := func(text string) (Settings, error) {
		var n yaml.Node
		if err := yaml.Unmarshal([]byte(text), &n); err != nil {
			t.Fatal(err)
		}
		return ParseSettings(*n.Content[0])
	}
	s, err := parse("{expectedRelease: v1, writes: {concurrency: {host: 16}}}")
	is(t, []any{err, s.ExpectedRelease, s.CheckStore, s.Writes.Concurrency}, []any{nil, "v1", true, Concurrency{8, 16, 8, 8}}, "defaults and an override")
	for _, bad := range []string{"{writes: {concurrency: {rack: 0}}}", "{writes: {concurrency: {powerShelf: -1}}}", "{writes: {concurency: {}}}",
		"{writes: {concurrency: {host: 1.5}}}", "{surprise: 1}"} {
		if _, err := parse(bad); err == nil || !strings.HasPrefix(err.Error(), "config settings") {
			t.Errorf("%s: %v", bad, err)
		}
	}
}

type ingestShiftClock struct{ shift time.Duration }

func (c *ingestShiftClock) Now() time.Time   { return time.Now().Add(c.shift) }
func (*ingestShiftClock) Instant() time.Time { return time.Now() }

type baselineBoundProbe struct{ *fakeProbe }

func (p baselineBoundProbe) Count(ctx context.Context, f probeapi.Filter) (probeapi.CountResponse, error) {
	time.Sleep(time.Millisecond)
	if err := ctx.Err(); err != nil {
		return probeapi.CountResponse{}, err
	}
	return p.fakeProbe.Count(ctx, f)
}

func TestReadinessEdgeAndMonotonicSendCutoff(t *testing.T) {
	for _, name := range []string{"last readiness poll", "backward wall clock"} {
		t.Run(name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRunRig(t)
				clock := &ingestShiftClock{}
				now := time.Now()
				rc := run.RunCtx{Ctx: context.Background(), Clock: clock, Deadline: now.Add(time.Hour), ProbeReadEnd: now.Add(2 * time.Hour), EvidenceEnd: now.Add(2 * time.Hour), DryRun: true}
				check := r.test.check
				if name == "last readiness poll" {
					r.test.probe = baselineBoundProbe{r.probe}
					r.test.check = func(ctx context.Context, d readiness.Deps, end time.Time) readiness.Report {
						time.Sleep(time.Until(end) - time.Nanosecond)
						return check(ctx, d, end)
					}
					out, err := r.test.Run(rc, r.w)
					if err != nil || out.Verdict != result.Go {
						t.Fatalf("last-poll gate=%s err=%v", out.Verdict, err)
					}
				} else {
					r.test.s.Writes.Concurrency = one
					clock.shift = time.Hour
					rc.Deadline = now.Add(time.Hour + 20*time.Second)
					rc.DryRun = false
					r.test.check = func(ctx context.Context, d readiness.Deps, end time.Time) readiness.Report {
						clock.shift = 0
						return check(ctx, d, end)
					}
					r.test.dial = func(string) (ForgeWriter, error) { time.Sleep(19 * time.Second); return r.f, nil }
					r.f.during = func(context.Context, string) { time.Sleep(2 * time.Second) }
					_, _ = r.test.Run(rc, r.w)
					if calls := r.f.seen(); len(calls) != 1 {
						t.Fatalf("sent past monotonic work end: %v", calls)
					}
				}
			})
		})
	}
}
