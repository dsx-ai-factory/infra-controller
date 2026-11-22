// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/fleetup"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/store"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// The fixture retains the real compact host/report/window shape. Snapshot
// timestamps advance only at the reader cadence, independently of writes.
type firmwareSite struct {
	runtimeOrdering                       []corev1.HostFirmwareComponentType
	closeDelay                            time.Duration
	sweepDuration, sweepInterval          time.Duration
	catalogPinLag                         time.Duration
	catalogConfigError, catalogUnitsError *probe.Error
	catalogErrorFor                       time.Duration
	catalogFailedReads                    int
	catalogMode                           string
	catalogRequests                       []*corev1.UpsertHostFirmwareConfigRequest
	catalogSent                           time.Time
	onProofRead                           func()
	onUnits                               func(probeapi.Family)
	sequence                              uint64
	snapshotEpoch                         float64
	sizes                                 []int
	sent                                  []time.Time
	onConfig                              func()
	requestOutageUntil                    time.Time
	onCensusFailure                       func()
	censusFailures                        int
	deadlineReads                         int
	outageUntil                           time.Time
	configOutage                          bool
	*instanceSite
	config probeapi.ConfigResponse
	writes int
	mode   string
	action func()
}

func firmwareFixture(n int) *firmwareSite {
	f := &firmwareSite{instanceSite: instanceFixture(n), runtimeOrdering: []corev1.HostFirmwareComponentType{1, 3}}
	f.state[probeapi.FamilyHostFirmwareRequests] = map[string]probeapi.Unit{}
	for id, u := range f.state[probeapi.FamilyHost] {
		h := u.Host
		h.Vendor = new("NVIDIA")
		h.Model = new("GB200 NVL")
		h.FirmwarePolicyEligible = new(true)
		h.FirmwareWindow = &probeapi.FirmwareWindow{}
		h.VersionSource = probeapi.VersionSourceExploredReport
		h.ReportAt = &jsonx.Time{Time: time.Now().Add(-time.Minute)}
		h.Firmware = &probeapi.HostFirmware{ReportVersion: new("before"), WaitingForRefresh: new(false), Components: []probeapi.FirmwareComponentInventory{
			{ComponentName: "bmc", Phase: probeapi.FirmwarePhaseBehind, Entries: []probeapi.FirmwareInventoryEntry{{ServiceID: "/redfish/v1/UpdateService", InventoryID: "/bmc-a", Version: new("old")}, {ServiceID: "/redfish/v1/UpdateService", InventoryID: "/bmc-b", Version: new("old")}}},
			{ComponentName: "uefi", Phase: probeapi.FirmwarePhaseCurrent, Entries: []probeapi.FirmwareInventoryEntry{{ServiceID: "/redfish/v1/UpdateService", InventoryID: "/uefi", Version: new("current")}}},
		}}
		f.state[probeapi.FamilyHost][id] = u
	}
	for id, u := range f.state[probeapi.FamilyDpu] {
		u.Done = true
		f.state[probeapi.FamilyDpu][id] = u
	}
	f.config = probeapi.ConfigResponse{Metrics: &probeapi.MetricsInfo{ContractVersion: 1, Selector: map[string]string{"job": "probe"}, ProcessStartSeconds: f.process, ScrapeIntervalSeconds: 15, ReaderIntervalsSeconds: map[probeapi.Family]float64{probeapi.FamilyHost: 30, probeapi.FamilyConfigNicoAPI: 30}}, NicoAPI: &probeapi.ConfigNicoAPI{Revision: new(uint64(7)), Runtime: probeapi.RuntimeSettings{AutoHostFirmwareUpdate: new(true), RequiresManualUpgrade: new(false), MachineUpdateRunIntervalSeconds: new(uint64(1)), MaxConcurrentMachineUpdatesAbsolute: new(int64(1000))}, FirmwareTargets: []probeapi.FirmwareTarget{
		{Vendor: "NVIDIA", Model: "GB200 NVL", Component: probeapi.ComponentBmc, ComponentName: new("bmc"), InventoryNameRegex: new("bmc"), Version: "target", ExplicitStart: true},
		{Vendor: "NVIDIA", Model: "GB200 NVL", Component: probeapi.ComponentUefi, ComponentName: new("uefi"), InventoryNameRegex: new("uefi"), Version: "current", ExplicitStart: true},
	}}}
	return f
}
func orderingTypes(refs []*corev1.HostFirmwareComponentRef) []corev1.HostFirmwareComponentType {
	types := make([]corev1.HostFirmwareComponentType, 0, len(refs))
	for _, ref := range refs {
		types = append(types, ref.GetType())
	}
	return types
}

func (f *firmwareSite) meta(family probeapi.Family) probeapi.SweepInfo {
	m := f.instanceSite.meta(family)
	if f.snapshotEpoch != 0 {
		m.SnapshotID = strings.Replace(m.SnapshotID, fmt.Sprintf("%.0f-", f.process*1000), fmt.Sprintf("%.0f-", f.snapshotEpoch*1000), 1)
	}
	m.SweepStart.Time = m.SweepStart.Add(15 * time.Second)
	m.ReadAt.Time = m.ReadAt.Add(15 * time.Second)
	if d := f.sweepDuration; d > 0 {
		end := time.Now().Truncate(d)
		if interval := f.sweepInterval; interval > 0 {
			origin := f.sent[0].Add(-time.Second)
			end = origin.Add(time.Since(origin)/interval*interval + d)
			if end.After(time.Now()) {
				end = end.Add(-interval)
			}
			m.SweepIntervalSeconds = interval.Seconds()
		}
		m.SweepStart.Time, m.ReadAt.Time = end.Add(-d).Add(10*time.Millisecond), end
		m.SnapshotID = fmt.Sprintf("%.0f-devices-%d", f.process*1000, end.UnixNano())
		m.SweepAgeSeconds = time.Since(end).Seconds()
	}
	if f.mode == "early snapshot" && len(f.sent) > 0 {
		m.SweepStart.Time = f.sent[0].Add(-time.Second)
		m.ReadAt.Time = m.SweepStart.Add(500 * time.Millisecond)
		f.sequence++
		m.SnapshotID += fmt.Sprint("-", f.sequence)
	}
	return m
}

func (f *firmwareSite) Config(ctx context.Context, _ probe.ConfigFilter) (probeapi.ConfigResponse, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.catalogMode == "read deadline" && !f.catalogSent.IsZero() {
		f.catalogFailedReads++
		<-ctx.Done()
		return probeapi.ConfigResponse{}, f.catalogConfigError
	}
	if !f.catalogSent.IsZero() && f.catalogConfigError != nil && (f.catalogErrorFor == 0 || time.Since(f.catalogSent) < f.catalogErrorFor) {
		f.catalogFailedReads++
		return probeapi.ConfigResponse{}, f.catalogConfigError
	}
	if f.configOutage && time.Now().Before(f.outageUntil) {
		return probeapi.ConfigResponse{}, &probe.Error{Kind: probe.Unavailable}
	}
	if f.onConfig != nil {
		f.onConfig()
	}
	b, _ := json.Marshal(f.config)
	var c probeapi.ConfigResponse
	if err := json.Unmarshal(b, &c); err != nil {
		return probeapi.ConfigResponse{}, &probe.Error{Kind: probe.Decode}
	}
	meta := f.meta(probeapi.FamilyHost)
	c.NicoAPI.ReadInfo = probeapi.ReadInfo{ReadStart: jsonx.Time{Time: meta.SweepStart.Add(-2 * time.Second)}, ReadAt: jsonx.Time{Time: meta.SweepStart.Add(-time.Second)}, ReadIntervalSeconds: 30, LastAttemptError: f.config.NicoAPI.LastAttemptError}
	if f.sweepDuration > 0 {
		end := time.Now().Add(-time.Second).Truncate(30 * time.Second)
		c.NicoAPI.ReadStart.Time, c.NicoAPI.ReadAt.Time = end, end.Add(time.Second)
	}
	if f.catalogMode == "pre-send proof" && !f.catalogSent.IsZero() {
		c.NicoAPI.FirmwareTargets[0].Version = "target"
		c.NicoAPI.ReadStart.Time = f.catalogSent.Add(-time.Second)
	}
	return c, nil
}
func (f *firmwareSite) Units(ctx context.Context, filter probeapi.Filter, _ *string, page probe.Page) (probeapi.UnitsResponse, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if !f.catalogSent.IsZero() && f.catalogUnitsError != nil && (f.catalogErrorFor == 0 || time.Since(f.catalogSent) < f.catalogErrorFor) {
		f.catalogFailedReads++
		return probeapi.UnitsResponse{}, f.catalogUnitsError
	}
	if time.Now().Before(f.outageUntil) {
		return probeapi.UnitsResponse{}, &probe.Error{Kind: probe.NoCompleteSweep}
	}
	var family probeapi.Family
	for _, spec := range firmwareFamilies {
		if spec.kind == filter.Type {
			family = spec.family
		}
	}
	if f.mode == "host read deadline" && family == probeapi.FamilyHost || f.mode == "census read deadline" && family == probeapi.FamilyHostFirmwareRequests {
		f.deadlineReads++
		<-ctx.Done()
		return probeapi.UnitsResponse{}, &probe.Error{Kind: probe.Cancelled}
	}
	if family == probeapi.FamilyHostFirmwareRequests && time.Now().Before(f.requestOutageUntil) {
		f.censusFailures++
		if f.onCensusFailure != nil {
			f.onCensusFailure()
		}
		return probeapi.UnitsResponse{}, &probe.Error{Kind: probe.NoCompleteSweep}
	}
	if f.mode == "census unavailable" && family == probeapi.FamilyHostFirmwareRequests {
		return probeapi.UnitsResponse{}, &probe.Error{Kind: probe.NoCompleteSweep}
	}
	meta := f.meta(family)
	if f.onUnits != nil {
		f.onUnits(family)
	}
	if family == probeapi.FamilyHost && len(f.sent) > 0 && meta.SweepStart.After(f.sent[0]) && f.onProofRead != nil {
		f.onProofRead()
	}
	meta.FirmwareConfig = &probeapi.FirmwareConfigEvidence{Revision: *f.config.NicoAPI.Revision, ReadStart: jsonx.Time{Time: meta.SweepStart.Add(-2 * time.Second)}, ReadAt: jsonx.Time{Time: meta.SweepStart.Add(-time.Second)}}
	if f.catalogPinLag > 0 && !f.catalogSent.IsZero() && !meta.SweepStart.After(f.catalogSent.Add(f.catalogPinLag)) {
		meta.FirmwareConfig.Revision = 7
	}
	rows := []probeapi.Unit{}
	for _, id := range slices.Sorted(maps.Keys(f.state[family])) {
		raw, _ := json.Marshal(f.state[family][id])
		var u probeapi.Unit
		if err := json.Unmarshal(raw, &u); err != nil {
			panic(err)
		}
		if u.Type == probeapi.UnitTypeHost && u.Host != nil && u.Host.FirmwareWindow != nil && u.Host.FirmwareWindow.Start != nil && f.writes > 0 && !closedFirmwareWindow(u.Host.FirmwareWindow) && meta.SweepStart.After(u.Host.FirmwareWindow.Start.Time) && f.mode != "no-upgrade" {
			u.Host.Firmware.Components[0].Phase = probeapi.FirmwarePhaseCurrent
			for i := range u.Host.Firmware.Components[0].Entries {
				u.Host.Firmware.Components[0].Entries[i].Version = new("target")
			}
			u.Host.Firmware.ReportVersion = new("after")
			u.Host.ReportAt = &meta.ReadAt
			u.Host.UpdateComplete = true
			u.Host.UpdateEligible = new(false)
		}
		rows = append(rows, u)
	}
	begin := 0
	if page.Cursor != nil {
		begin, _ = strconv.Atoi(*page.Cursor)
	}
	end := min(begin+1000, len(rows))
	reply := probeapi.UnitsResponse{SweepInfo: meta, Units: rows[begin:end]}
	if end < len(rows) {
		reply.NextCursor = new(strconv.Itoa(end))
	}
	return reply, nil
}
func (f *firmwareSite) Count(ctx context.Context, filter probeapi.Filter) (probeapi.CountResponse, error) {
	rows, err := f.Units(ctx, filter, nil, probe.Page{})
	n := uint64(0)
	for _, u := range rows.Units {
		if filter.FirmwareComponent == nil || u.Host != nil && u.Host.Firmware != nil && u.Host.Firmware.Components[0].Phase == probeapi.FirmwarePhaseCurrent && readyHost(u) {
			n++
		}
	}
	return probeapi.CountResponse{Count: n, SweepInfo: rows.SweepInfo}, err
}
func (f *firmwareSite) Close() error { time.Sleep(f.closeDelay); return nil }
func (f *firmwareSite) UpsertHostFirmwareConfig(_ context.Context, r *corev1.UpsertHostFirmwareConfigRequest) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.catalogRequests = append(f.catalogRequests, proto.Clone(r).(*corev1.UpsertHostFirmwareConfigRequest))
	f.catalogSent = time.Now()
	if f.catalogMode == "denied" {
		return status.Error(codes.PermissionDenied, "private upstream text")
	}
	if f.catalogMode == "absent" || f.catalogMode == "pre-send proof" {
		return status.Error(codes.Unavailable, "lost")
	}
	// Core validates the merged runtime row before overlaying static targets.
	components := map[corev1.HostFirmwareComponentType]bool{}
	for _, c := range f.runtimeOrdering {
		components[c] = true
	}
	for _, c := range r.Components {
		components[c.Type] = true
	}
	ordering := f.runtimeOrdering
	if len(r.ComponentOrdering) > 0 {
		ordering = orderingTypes(r.ComponentOrdering)
	}
	seen := map[corev1.HostFirmwareComponentType]bool{}
	for _, c := range ordering {
		if seen[c] || !components[c] {
			return status.Error(codes.InvalidArgument, "runtime ordering mismatch")
		}
		seen[c] = true
	}
	if len(seen) == 0 || len(seen) != len(components) {
		return status.Error(codes.InvalidArgument, "runtime ordering incomplete")
	}
	f.runtimeOrdering = slices.Clone(ordering)
	found := false
	for i := range f.config.NicoAPI.FirmwareTargets {
		target := &f.config.NicoAPI.FirmwareTargets[i]
		if target.Vendor == r.Vendor && target.Model == r.Model && strings.EqualFold(*target.ComponentName, strings.TrimPrefix(r.Components[0].Type.String(), "HOST_FIRMWARE_COMPONENT_TYPE_")) {
			found = true
			target.Version = r.Components[0].Firmware[0].Version
			target.ExplicitStart = *r.ExplicitStartNeeded
		}
	}
	if !found {
		name := strings.ToLower(strings.TrimPrefix(r.Components[0].Type.String(), "HOST_FIRMWARE_COMPONENT_TYPE_"))
		f.config.NicoAPI.FirmwareTargets = append(f.config.NicoAPI.FirmwareTargets, probeapi.FirmwareTarget{Vendor: r.Vendor, Model: r.Model, Component: probeapi.Component(name), ComponentName: new(name), InventoryNameRegex: new(name), Version: r.Components[0].Firmware[0].Version, ExplicitStart: *r.ExplicitStartNeeded})
		inventory := firmwareFixture(len(f.state[probeapi.FamilyHost]))
		for id, u := range f.state[probeapi.FamilyHost] {
			if u.Host.Firmware == nil {
				u.Host.Firmware = inventory.state[probeapi.FamilyHost][id].Host.Firmware
				u.Host.Firmware.Components = slices.DeleteFunc(u.Host.Firmware.Components, func(g probeapi.FirmwareComponentInventory) bool { return g.ComponentName != name })
			}
		}
	}
	*f.config.NicoAPI.Revision++
	if f.catalogMode == "conflict" {
		f.config.NicoAPI.FirmwareTargets[1].Version = "foreign"
	}
	if f.catalogMode == "lost" {
		return status.Error(codes.Unavailable, "lost")
	}
	return nil
}

func (f *firmwareSite) SetFirmwareUpdateTimeWindow(_ context.Context, r *corev1.SetFirmwareUpdateTimeWindowRequest) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.writes++
	f.sizes = append(f.sizes, len(r.MachineIds))
	f.sent = append(f.sent, time.Now())
	if f.action != nil {
		f.action()
	}
	if strings.HasPrefix(f.mode, "lost second") && f.writes == 2 || f.mode == "late commit" && f.writes == 1 {
		return status.Error(codes.Unavailable, "lost")
	}
	if f.mode == "rejected" {
		return status.Error(codes.PermissionDenied, "private server URL")
	}
	if f.mode != "missing" {
		for i, id := range r.MachineIds {
			if (f.mode == "partial" || f.mode == "partial lost") && i > 0 {
				continue
			}
			u := f.state[probeapi.FamilyHost][id.Id]
			u.Host.FirmwareWindow = &probeapi.FirmwareWindow{Start: &jsonx.Time{Time: r.StartTimestamp.AsTime()}, End: &jsonx.Time{Time: r.EndTimestamp.AsTime()}}
			if f.mode == "foreign" {
				u.Host.FirmwareWindow.End.Time = u.Host.FirmwareWindow.End.Add(time.Second)
			}
			f.state[probeapi.FamilyHost][id.Id] = u
		}
	}
	if f.mode == "lost" || f.mode == "partial lost" || f.mode == "lost second close lost" && f.writes == 3 {
		return status.Error(codes.Unavailable, "private server URL")
	}
	return nil
}

type firmwareMetrics struct {
	unhealthy atomic.Bool
	fakeStore
	mu            sync.Mutex
	mode          string
	baselineDelay time.Duration
	completed     func() time.Time
	managerCalls  int
	afterManager  func(int)
}

func (m *firmwareMetrics) PoolHealthy(context.Context, time.Time, map[string]string) ([]string, error) {
	if m.unhealthy.Load() {
		return nil, instanceIssue("unhealthy pool")
	}
	return []string{"api"}, nil
}
func (m *firmwareMetrics) FirmwareManagers(context.Context, time.Time, map[string]string, time.Duration, time.Time) (map[string]store.FirmwareManager, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.managerCalls++
	if m.afterManager != nil {
		m.afterManager(m.managerCalls)
	}
	v := uint64(time.Now().Unix() / 30)
	if m.mode == "heartbeat" {
		v = 1
	}
	at := time.Now().Truncate(30 * time.Second)
	from := at.Add(-90 * time.Second)
	if m.mode == "manager gap" {
		from = at
	}
	if m.mode == "pre-completion increments" && !m.completed().IsZero() {
		v = 12
		if m.managerCalls == 2 {
			v = 10
			at = m.completed().Add(-time.Second)
		}
	}
	if m.mode == "manager reset" {
		v = 10
		if m.managerCalls == 3 {
			v = 0
		} else if m.managerCalls > 3 {
			v = uint64(7 + m.managerCalls)
		}
	}
	rows := map[string]store.FirmwareManager{"core/api-0": {Passes: v, Cap: 1000, From: from, At: at}}
	if m.mode == "lock loser first" {
		rows["core/api-1"] = rows["core/api-0"]
		v := rows["core/api-0"]
		v.Passes = 1
		rows["core/api-0"] = v
	}
	return rows, nil
}
func (m *firmwareMetrics) FirmwareSummary(_ context.Context, _ time.Time, _ probeapi.Filter, p probeapi.SummaryParams, _ uint64) (probeapi.SummaryResponse, error) {
	if p.N == 0 {
		time.Sleep(m.baselineDelay)
		switch m.mode {
		case "baseline timeout":
			return probeapi.SummaryResponse{}, context.DeadlineExceeded
		case "baseline canceled":
			return probeapi.SummaryResponse{}, context.Canceled
		case "baseline mismatch":
			return probeapi.SummaryResponse{FinalCount: new(p.Base.Count + 1)}, nil
		}
	}
	c := p.Base.Count + p.N
	r := probeapi.SummaryResponse{FinalCount: &c, FinalScrapeSeen: true, TimingErrorSeconds: new(31.0), N: p.N}
	for _, q := range probeapi.FixedQuantiles {
		r.Crossings = append(r.Crossings, probeapi.Crossing{Q: q, Seconds: new(30.0)})
	}
	return r, nil
}
func firmwareHarness(t *testing.T, site *firmwareSite) (*FirmwareTest, *rig, *run.Control) {
	t.Helper()
	dir := t.TempDir()
	r := newRig(t)
	ctl := run.NewControl()
	t.Cleanup(ctl.Cancel)
	r.w.Close()
	r.w = writer.New(r.j, r.gate, ctl.Stopper(), time.Time{})
	cfg := &config.Config{RunID: "fw", Command: "firmware", Namespace: "mat", Site: config.Site{LockName: "site-lock"}, Journal: config.Journal{Dir: dir}, ControlPlane: []config.PodSelector{{Namespace: "core", LabelSelector: "api"}}, Probe: config.Probe{PollSeconds: 5}, Timeouts: config.Timeouts{ReadinessSeconds: 300}}
	s := FirmwareSettings{Settings: fleetup.DefaultSettings()}
	s.Mat.Selector = "mat"
	s.NativeHealth.APIPoolSelector = map[string]string{"job": "api"}
	s.NativeHealth.APIPoolReplicas = 1
	s.Observation.IntervalSeconds = 30
	s.Firmware.N = uint64(len(site.state[probeapi.FamilyHost]))
	s.Firmware.Component = probeapi.ComponentBmc
	s.Firmware.TargetVersion = "target"
	s.Firmware.ArtifactURL = firmwareArtifactURL
	s.Firmware.ArtifactSHA256 = firmwareArtifactSHA256
	s.Firmware.WindowSeconds = 3600
	s.Firmware.CleanupSeconds = 360
	s.Firmware.ReportMaxAgeSeconds = 600
	s.Firmware.ManagerScrapeSeconds = 30
	pods := &fakePods{pods: map[string][]cluster.PodInfo{"mat": {{Name: "mat-0", UID: "uid-1", Ready: true}}, "core": {{Name: "api-0", UID: "uid-2", Ready: true}}}}
	metrics := &firmwareMetrics{}
	deps := readiness.Deps{Pods: pods, Store: func(context.Context) (readiness.StoreProbe, error) { return readiness.StoreProbe{Client: metrics}, nil }}
	test := &FirmwareTest{cfg: cfg, s: s, deps: deps, p: site, check: func(context.Context, readiness.Deps, time.Time) readiness.Report {
		return readiness.Report{Checks: []readiness.Check{{Name: "fixture", OK: true}}, Inventory: site.inv}
	}, dial: func(string) (firmwareWriter, error) { return site, nil }}
	return test, r, ctl
}
func firmwareRunTest(test *FirmwareTest, r *rig, ctl *run.Control, dry bool) (result.Outcome, error) {
	now := time.Now()
	return test.Run(run.RunCtx{Ctx: ctl.Context(), Clock: run.RealClock{}, Deadline: now.Add(time.Hour), ProbeReadEnd: now.Add(2 * time.Hour), DryRun: dry}, r.w)
}

func TestFirmwarePopulatedReadinessClock(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		test, r, ctl := firmwareHarness(t, site)
		test.check = readiness.Populated
		test.deps.Probe = site
		test.deps.Records = inventory.NewHTTPReader()
		test.deps.Settings = readiness.Settings{Namespace: "mat", MatSelector: "mat", ControlPlane: test.cfg.ControlPlane}
		// Real readiness must refuse the MAT pod's missing IP without panicking.
		out, err := firmwareRunTest(test, r, ctl, true)
		if err != nil || out.Verdict != result.NotReady || !bytes.Contains(out.Detail, []byte("has no IP")) || site.writes != 0 || len(site.catalogRequests) != 0 {
			t.Fatalf("readiness: verdict=%s err=%v detail=%s", out.Verdict, err, out.Detail)
		}
	})
}

func TestFirmwareAdmission(t *testing.T) {
	for _, mode := range []string{"no prior lifecycle", "different baselines", "partial Ready fleet", "instance", "short cohort", "manual", "zero cap", "unknown policy", "foreign window", "unknown window", "empty inventory", "mixed inventory", "partial target", "null version", "wrong model", "multiple vendors", "late request", "late request dry"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(2)
				test, r, ctl := firmwareHarness(t, site)
				h := site.state[probeapi.FamilyHost]["h0"].Host
				switch mode {
				case "different baselines":
					for i := range h.Firmware.Components[0].Entries {
						h.Firmware.Components[0].Entries[i].Version = new("older")
					}
				case "instance":
					site.state[probeapi.FamilyInstance]["foreign"] = probeapi.Unit{ID: "foreign", Type: probeapi.UnitTypeInstance, Source: probeapi.SourceNicoAPI, Stage: probeapi.StageRegistered, State: "Pending"}
				case "partial Ready fleet":
					h.Allocatable = new(false)
					test.s.Firmware.N = 1
				case "short cohort":
					h.Allocatable = new(false)
				case "manual":
					site.config.NicoAPI.Runtime.RequiresManualUpgrade = new(true)
				case "zero cap":
					site.config.NicoAPI.Runtime.MaxConcurrentMachineUpdatesAbsolute = new(int64(0))
				case "unknown policy":
					site.config.NicoAPI.Runtime.AutoHostFirmwareUpdate = nil
				case "foreign window":
					h.FirmwareWindow = &probeapi.FirmwareWindow{Start: &jsonx.Time{Time: time.Now().Add(-time.Hour)}, End: &jsonx.Time{Time: time.Now().Add(-time.Minute)}}
				case "unknown window":
					h.FirmwareWindow = nil
				case "empty inventory":
					h.Firmware.Components[0].Entries = nil
				case "mixed inventory":
					h.Firmware.Components[0].Entries[1].Version = new("different")
				case "partial target":
					h.Firmware.Components[0].Entries[1].Version = new("target")
				case "null version":
					h.Firmware.Components[0].Entries[1].Version = nil
				case "multiple vendors":
					site.state[probeapi.FamilyHost]["h1"].Host.Vendor = new("other")
					for _, target := range slices.Clone(site.config.NicoAPI.FirmwareTargets) {
						target.Vendor = "other"
						site.config.NicoAPI.FirmwareTargets = append(site.config.NicoAPI.FirmwareTargets, target)
					}
				case "late request", "late request dry":
					start := time.Now()
					site.onUnits = func(family probeapi.Family) {
						if family == probeapi.FamilyHostFirmwareRequests && (time.Since(start) >= 30*time.Second || mode == "late request dry") {
							site.state[family]["h0"] = probeapi.Unit{ID: "h0", Type: probeapi.UnitTypeHostFirmwareRequest, Source: probeapi.SourceNicoAPI}
						}
					}
				case "wrong model":
					for _, u := range site.state[probeapi.FamilyHost] {
						u.Host.Model = new("unsupported")
					}
					for i := range site.config.NicoAPI.FirmwareTargets {
						site.config.NicoAPI.FirmwareTargets[i].Model = "unsupported"
					}
				}
				if mode == "wrong model" {
					snapshot := &firmwareSnapshot{Rows: site.state}
					for _, u := range site.state[probeapi.FamilyHost] {
						if firmwareStructural(snapshot, u, test.s) || firmwareInventory(u, site.config, test.s.Firmware.Component, test.s.Firmware.TargetVersion, true) {
							t.Fatal("unsupported model admitted by a boundary")
						}
					}
				}
				valid := mode == "instance" || mode == "no prior lifecycle" || mode == "different baselines" || mode == "partial Ready fleet"
				out, err := firmwareRunTest(test, r, ctl, valid || mode == "late request dry")
				if (out.Verdict == result.Passed) != valid || !valid && out.Verdict != result.NotReady || valid && err != nil || site.writes != 0 {
					t.Fatalf("verdict=%s err=%v writes=%d", out.Verdict, err, site.writes)
				}

			})
		})
	}
}

func TestFirmwareWithInstances(t *testing.T) {
	for _, n := range []uint64{2, 3} {
		t.Run(fmt.Sprint(n), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(3)
				u := site.state[probeapi.FamilyHost]["h0"]
				u.Stage, u.State, u.Reason, u.Done = probeapi.StageRegistered, "Assigned", new("Assigned/Ready"), false
				u.Host.Allocatable = new(false)
				site.state[probeapi.FamilyHost]["h0"] = u
				site.state[probeapi.FamilyInstance]["foreign"] = probeapi.Unit{ID: "foreign", Type: probeapi.UnitTypeInstance, Source: probeapi.SourceNicoAPI, Stage: probeapi.StageReady, State: "Ready"}
				site.onUnits = func(family probeapi.Family) {
					if family == probeapi.FamilyInstance {
						t.Fatal("firmware must not depend on the instance family")
					}
				}
				test, r, ctl := firmwareHarness(t, site)
				test.s.Firmware.N = n
				out, err := firmwareRunTest(test, r, ctl, false)
				if n == 2 {
					if err != nil || out.Verdict != result.Passed || site.writes != 1 || len(test.wave.plan.Cohort) != 2 {
						t.Fatalf("free cohort: verdict=%s err=%v writes=%d reasons=%v", out.Verdict, err, site.writes, out.Reasons)
					}
					for _, h := range test.wave.plan.Cohort {
						if h.ID == "h0" {
							t.Fatal("occupied host selected")
						}
					}
				} else if out.Verdict != result.NotReady || out.ExitCode != 2 || site.writes != 0 || len(site.catalogRequests) != 0 {
					t.Fatalf("shortfall: verdict=%s err=%v writes=%d", out.Verdict, err, site.writes)
				}
				if !closedFirmwareWindow(site.state[probeapi.FamilyHost]["h0"].Host.FirmwareWindow) {
					t.Fatal("opened a window on an occupied host")
				}
			})
		})
	}
}

func TestFirmwareReprovisionSequence(t *testing.T) {
	for _, mode := range []string{"healthy", "host failed", "firmware failed", "firmware failed after deadline", "stop during reset"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				if mode == "firmware failed after deadline" {
					site.closeDelay = 2 * time.Hour
				}
				site.mode = "no-upgrade"
				test, r, ctl := firmwareHarness(t, site)
				states := []string{"WaitingForUpload", "WaitingForFirmwareUpgrade", "ResetForNewFirmware", "NewFirmwareReportedWait", "CheckingFirmwareRepeatV2"}
				seen := map[string]bool{}
				site.onUnits = func(family probeapi.Family) {
					if family != probeapi.FamilyHost || site.writes == 0 {
						return
					}
					u := site.state[probeapi.FamilyHost]["h0"]
					if u.Host.Firmware == nil {
						return
					}
					step := int(time.Since(site.sent[0]) / (30 * time.Second))
					if step < len(states) {
						seen[states[step]] = true
						u.Stage, u.State, u.Done = probeapi.StageRegistered, "HostReprovisioning", false
						u.Reason = new("HostReprovisioning/" + states[step])
						u.Host.Allocatable = new(false)
						// Probe reports missing exploration evidence during BMC reset as Unknown.
						u.Host.ReportError = nil
						if step == 2 || step == 3 {
							u.Host.ReportError = new("RedfishError")
						}
						for i := range u.Host.Firmware.Components {
							u.Host.Firmware.Components[i].Phase = probeapi.FirmwarePhaseUpdating
							if u.Host.ReportError != nil {
								u.Host.Firmware.Components[i].Phase = probeapi.FirmwarePhaseUnknown
							}
						}
						if step == 2 && mode == "stop during reset" {
							ctl.RequestStop()
						}
						if step >= 3 && (mode == "host failed" || strings.HasPrefix(mode, "firmware failed")) {
							u.Host.ReportError = nil
							u.Reason = new("HostReprovisioning/FailedFirmwareUpgrade { firmware_type: Bmc, report_time: None, reason: None }")
							if mode == "host failed" {
								u.Stage, u.State = probeapi.StageError, "Failed"
								u.Reason = new("Failed/Reprovisioning")
								u.Host.Firmware = nil
							} else {
								for i := range u.Host.Firmware.Components {
									u.Host.Firmware.Components[i].Phase = probeapi.FirmwarePhaseFailed
								}
							}
						}
					} else {
						u.Stage, u.State, u.Done, u.Reason = probeapi.StageReady, "Ready", true, nil
						u.Host.Allocatable, u.Host.ReportError = new(true), nil
						u.Host.UpdateComplete, u.Host.UpdateEligible = true, new(false)
						u.Host.ReportAt = new(site.meta(family).ReadAt)
						u.Host.Firmware.ReportVersion = new("after")
						for i := range u.Host.Firmware.Components {
							u.Host.Firmware.Components[i].Phase = probeapi.FirmwarePhaseCurrent
						}
						for i := range u.Host.Firmware.Components[0].Entries {
							u.Host.Firmware.Components[0].Entries[i].Version = new("target")
						}
					}
					site.state[probeapi.FamilyHost]["h0"] = u
				}
				out, err := firmwareRunTest(test, r, ctl, false)
				if err != nil && (mode != "stop during reset" || !errors.Is(err, writer.ErrSoftStop)) || site.writes != 1 {
					t.Fatalf("run: err=%v writes=%d reasons=%v", err, site.writes, out.Reasons)
				}
				if mode == "stop during reset" {
					if out.Verdict != result.Unresolved || out.Stages[0].Units.Failed != 0 || !test.wave.completed.IsZero() {
						t.Fatalf("unfinished is not failed: %+v %v", out.Stages, out.Reasons)
					}
				} else if mode == "healthy" {
					if out.Verdict != result.Passed || out.Stages[0].Units.Failed != 0 || len(seen) != len(states) || test.wave.completed.IsZero() {
						t.Fatalf("normal reprovision: verdict=%s units=%+v states=%v reasons=%v", out.Verdict, out.Stages[0].Units, seen, out.Reasons)
					}
				} else if out.Verdict != result.Failed || out.Stages[0].Units.Failed != 1 || !strings.Contains(fmt.Sprint(out.Reasons), "firmware cohort") || strings.Contains(fmt.Sprint(out.Reasons), "instance") {
					t.Fatalf("terminal failure: verdict=%s units=%+v reasons=%v", out.Verdict, out.Stages[0].Units, out.Reasons)
				}
			})
		})
	}
}

func TestFirmwarePostCatalogMissingHost(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		test, r, ctl := firmwareHarness(t, site)
		site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
		start := time.Now()
		site.onUnits = func(family probeapi.Family) {
			if family == probeapi.FamilyHost && len(site.catalogRequests) > 0 && time.Since(start) >= 30*time.Second {
				u := site.state[family]["h0"]
				u.Host = nil
				raw, err := json.Marshal(u)
				if err != nil {
					t.Fatal(err)
				}
				if err = jsonx.Decode(raw, &u); err != nil {
					t.Fatal(err)
				}
				site.state[family]["h0"] = u
			}
		}
		out, err := firmwareRunTest(test, r, ctl, false)
		if err != nil || out.Verdict != result.NotReady || site.writes != 0 || len(site.catalogRequests) != 1 {
			t.Fatalf("admission: verdict=%s err=%v windows=%d catalog=%d", out.Verdict, err, site.writes, len(site.catalogRequests))
		}
	})
}

func TestFirmwareUnknownAfterCompletion(t *testing.T) {
	for _, phase := range []string{"observer", "post-pass", "observer refresh", "post-pass refresh", "observer refresh stuck", "post-pass terminal"} {
		t.Run(phase, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				test, r, ctl := firmwareHarness(t, site)
				if out, err := firmwareRunTest(test, r, ctl, false); err != nil || out.Verdict != result.Passed {
					t.Fatal(out, err)
				}
				f := test.wave
				site.mode = "no-upgrade"
				site.state[probeapi.FamilyHost]["h0"] = f.latest.Rows[probeapi.FamilyHost]["h0"]
				var until time.Time
				site.onUnits = func(family probeapi.Family) {
					if family != probeapi.FamilyHost {
						return
					}
					// Run joined its checker, keep healthy evidence fresh for this observer-only call.
					f.health.Store(&instanceHealth{at: time.Now()})
					if until.IsZero() {
						until = time.Now().Add(time.Minute)
						if strings.HasSuffix(phase, "stuck") {
							until = until.Add(3 * time.Minute)
						}
					}
					h := site.state[family]["h0"].Host
					h.ReportError = nil
					h.ReportAt = new(site.meta(family).ReadAt)
					h.Firmware.WaitingForRefresh = new(strings.Contains(phase, "refresh") && time.Now().Before(until))
					for i := range h.Firmware.Components {
						h.Firmware.Components[i].Phase = probeapi.FirmwarePhaseCurrent
						if phase == "post-pass terminal" {
							h.Firmware.Components[i].Phase = probeapi.FirmwarePhaseFailed
						} else if time.Now().Before(until) && !strings.Contains(phase, "refresh") {
							h.ReportError = new("RedfishError")
							h.Firmware.Components[i].Phase = probeapi.FirmwarePhaseUnknown
						}
					}
				}
				verify := f.observeCompletion
				if strings.HasPrefix(phase, "post-pass") {
					verify = f.verifyPasses
				}
				end := time.Now().Add(3 * time.Minute)
				err := verify(context.Background(), end, r.w)
				if strings.HasSuffix(phase, "stuck") {
					if err == nil || time.Now().Before(end) || len(f.done) != 0 {
						t.Fatalf("refresh was not bounded: err=%v done=%d", err, len(f.done))
					}
				} else if phase == "post-pass terminal" {
					if !errors.Is(err, instanceIssue("a firmware cohort host or DPU failed")) {
						t.Fatalf("lost terminal failure: %v", err)
					}
				} else if err != nil || until.IsZero() || time.Now().Before(until) {
					t.Fatalf("did not wait for healthy evidence: %v", err)
				}
			})
		})
	}
}

func TestFirmwareWindowReceipts(t *testing.T) {
	for _, mode := range []string{"accepted", "lost", "missing", "rejected", "partial", "partial lost", "lost second", "lost second close lost", "late commit", "foreign", "early snapshot", "proof fsync failure"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				n := 1
				if mode == "partial" || mode == "partial lost" {
					n = 2
				}
				if strings.HasPrefix(mode, "lost second") {
					n = 101
				}
				site := firmwareFixture(n)
				site.mode = mode
				test, r, ctl := firmwareHarness(t, site)
				if mode == "proof fsync failure" {
					site.onProofRead = func() {
						r.j.Inject(func(_ *os.File, line []byte) error {
							if bytes.Contains(line, []byte("firmwareWindowReadBack")) {
								return errors.New("fsync failure")
							}
							return nil
						})
					}
				}
				out, err := firmwareRunTest(test, r, ctl, false)
				if strings.HasPrefix(mode, "lost second") && (time.Since(test.wave.attempts[1].Outcome.SentAt) > 65*time.Second || !strings.Contains(strings.Join(out.Reasons, " "), "deadline")) {
					t.Fatalf("opening read-back bound/cause: %v %v", time.Since(test.wave.attempts[1].Outcome.SentAt), out.Reasons)
				}
				if mode == "lost second close lost" {
					site.action = func() {
						if site.writes == 3 {
							site.outageUntil = time.Now().Add(65 * time.Second)
						}
					}
				}
				want := mode == "accepted" || mode == "lost"
				opens := 1
				if strings.HasPrefix(mode, "lost second") {
					opens = 2
				}
				if (out.Verdict == result.Passed) != want || want && err != nil || site.writes != opens {
					t.Fatalf("verdict=%s err=%v opens=%d reasons=%v", out.Verdict, err, site.writes, out.Reasons)
				}
				if want && !r.j.Unsettled() {
					t.Fatal("accepted windows lost their durable pending marker")
				}
				if mode == "proof fsync failure" && !errors.Is(err, journal.ErrIO) {
					t.Fatal("journal failure lost", err)
				}
				if mode == "late commit" {
					// A commit appears only on the additional sweep during cleanup.
					cleanupStart := time.Now()
					site.onProofRead = func() {
						if !test.wave.cleaning {
							return
						}
						if site.meta(probeapi.FamilyHost).SweepStart.After(cleanupStart) {
							for _, u := range site.state[probeapi.FamilyHost] {
								u.Host.FirmwareWindow = &probeapi.FirmwareWindow{Start: &jsonx.Time{Time: time.Unix(test.wave.plan.Start, 0)}, End: &jsonx.Time{Time: time.Unix(test.wave.plan.End, 0)}}
							}
							site.onProofRead = nil
						}
					}
				}
				ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
				defer cancel()
				cleanup := writer.NewFirmwareCleanup(r.j, r.gate, time.Now().Add(3*time.Minute))
				defer cleanup.Close()
				_, closeErr := test.CloseFirmwareWindows(ctx, cleanup)
				if want && (closeErr != nil || r.j.Unsettled() || site.writes != 2) {
					t.Fatalf("cleanup=%v pending=%v writes=%d", closeErr, r.j.Unsettled(), site.writes)
				}
				if mode == "late commit" && (closeErr != nil || site.writes != 2 || r.j.Unsettled()) {
					t.Fatalf("late commit cleanup=%v writes=%d pending=%v", closeErr, site.writes, r.j.Unsettled())
				}
				if strings.HasPrefix(mode, "lost second") || mode == "partial lost" {
					if closeErr == nil || !r.j.Unsettled() || r.j.Counts().Unclear != 1 || site.writes != opens+1 {
						t.Fatalf("unresolved open lost: %v pending=%v unclear=%d writes=%d", closeErr, r.j.Unsettled(), r.j.Counts().Unclear, site.writes)
					}
					for _, u := range site.state[probeapi.FamilyHost] {
						if !closedFirmwareWindow(u.Host.FirmwareWindow) {
							t.Fatal("proven owned window left open")
						}
					}
				}
				if mode == "missing" && (!r.j.Unsettled() || site.writes != 1) {
					t.Fatal("absent opening replayed or declared clean")
				}
			})
		})
	}
}

func TestFirmwareWriteEvidence(t *testing.T) {
	for _, mode := range []string{"valid", "in progress", "end-aged sweeps", "stale health", "failed health", "stale host", "stale config", "stopped", "changed identity", "DPU binding", "revision change", "unknown phase", "unrecognized phase", "missing inventory", "foreign window", "DPU error", "cleanup valid", "cleanup foreign", "cleanup identity", "cleanup unknown"} {
		t.Run(mode, func(t *testing.T) {
			site := firmwareFixture(1)
			test, _, _ := firmwareHarness(t, site)
			s, err := readFirmware(context.Background(), site, time.Now().Add(time.Minute), 30, false)
			if err != nil {
				t.Fatal(err)
			}
			u := s.Rows[probeapi.FamilyHost]["h0"]
			f := &firmwareRun{t: test, latest: s, stopped: make(chan struct{}), plan: firmwarePlan{Config: s.Config, Cohort: []probeapi.Unit{u}, Identity: map[string]string{"h0": firmwareIdentity(s, u)}}}
			f.health.Store(&instanceHealth{at: time.Now()})
			switch mode {
			case "end-aged sweeps":
				s.Config.NicoAPI.ReadAt.Time = time.Now().Add(-55 * time.Second)
				s.Config.NicoAPI.ReadStart.Time = s.Config.NicoAPI.ReadAt.Add(-20 * time.Second)
				for family, meta := range s.Sources {
					meta.ReadAt.Time = time.Now().Add(-55 * time.Second)
					meta.SweepStart.Time = meta.ReadAt.Add(-20 * time.Second)
					s.Sources[family] = meta
				}
			case "in progress":
				f.opened = map[string]bool{"h0": true}
				u.Stage = probeapi.StageRegistered
				u.Done = false
				u.State = "Maintenance"
				u.Host.Allocatable = new(false)
				u.Host.Firmware.Components[0].Phase = probeapi.FirmwarePhaseUpdating
				s.Rows[probeapi.FamilyHost][u.ID] = u
			case "stale config":
				s.Config.NicoAPI.ReadStart.Time = time.Now().Add(-62 * time.Second)
				s.Config.NicoAPI.ReadAt.Time = time.Now().Add(-61 * time.Second)
			case "DPU binding":
				s.Rows[probeapi.FamilyDpu]["d0"].Dpu.HostID = new("foreign")
			case "unknown phase", "unrecognized phase":
				f.opened = map[string]bool{"h0": true}
				u.Host.Firmware.Components[0].Phase = probeapi.FirmwarePhaseUnknown
				if mode == "unrecognized phase" {
					u.Host.Firmware.Components[0].Phase = probeapi.FirmwarePhase("future")
				}
			case "missing inventory":
				f.opened = map[string]bool{"h0": true}
				u.Host.Firmware = nil
			case "revision change":
				site.config.NicoAPI.Revision = new(uint64(8))
				if err = f.refresh(context.Background(), time.Now().Add(time.Minute)); err == nil {
					t.Fatal("revision change accepted")
				}
				return
			case "stale health":
				f.health.Store(&instanceHealth{at: time.Now().Add(-15 * time.Second)})
			case "failed health":
				f.health.Store(&instanceHealth{at: time.Now(), err: errors.New("unhealthy")})
			case "stale host":
				m := s.Sources[probeapi.FamilyHost]
				m.SweepStart.Time = time.Now().Add(-62 * time.Second)
				m.ReadAt.Time = time.Now().Add(-61 * time.Second)
				s.Sources[probeapi.FamilyHost] = m
			case "stopped":
				f.latch()
			case "changed identity":
				u.BmcIP = new("192.0.2.99")
				s.Rows[probeapi.FamilyHost][u.ID] = u
			case "foreign window":
				u.Host.FirmwareWindow = &probeapi.FirmwareWindow{Start: &jsonx.Time{Time: time.Now()}, End: &jsonx.Time{Time: time.Now().Add(time.Hour)}}
			case "DPU error":
				d := s.Rows[probeapi.FamilyDpu]["d0"]
				d.Stage = probeapi.StageError
				s.Rows[probeapi.FamilyDpu]["d0"] = d
			}
			cleanup := strings.HasPrefix(mode, "cleanup ")
			if cleanup {
				f.cleaning = true
				f.opened = map[string]bool{"h0": true}
				f.plan.Start = 1
				f.plan.End = 2
				u.Host.FirmwareWindow = &probeapi.FirmwareWindow{Start: &jsonx.Time{Time: time.Unix(1, 0)}, End: &jsonx.Time{Time: time.Unix(2, 0)}}
				if mode == "cleanup foreign" {
					u.Host.FirmwareWindow.End.Time = time.Unix(3, 0)
				}
				if mode == "cleanup identity" {
					u.BmcIP = new("192.0.2.99")
				}
				if mode == "cleanup unknown" {
					u.Host.FirmwareWindow = nil
				}
				s.Rows[probeapi.FamilyHost][u.ID] = u
			}
			if err = f.writeGuard([]string{"h0"}, cleanup); (err == nil) != (mode == "valid" || mode == "in progress" || mode == "end-aged sweeps" || mode == "unknown phase" || mode == "missing inventory" || mode == "cleanup valid") {
				t.Fatalf("guard=%v", err)
			}
		})
	}
}

func TestFirmwareSettings(t *testing.T) {
	for _, tc := range []struct {
		field string
		value any
	}{
		{"valid", nil}, {"default cadence", 480}, {"n", 0}, {"n", -1}, {"n", uint64(probeapi.MaxCount + 1)},
		{"windowSeconds", uint64(18446744073709551615)}, {"windowSeconds", 0},
		{"reportMaxAgeSeconds", uint64(18446744073709551615)}, {"reportMaxAgeSeconds", 0},
		{"managerScrapeSeconds", 0}, {"managerScrapeSeconds", 3601}, {"cleanupSeconds", 0}, {"cleanupSeconds", 3601}, {"cleanupSeconds", 60}, {"cleanupSeconds", 150},
		{"component", "nic"},
		{"artifactURL", "http://untrusted.invalid/image"}, {"artifactURL", "https://user:secret@host.invalid/image"}, {"artifactSHA256", "wrong"}, {"artifactSHA256", "aa"},
		{"targetVersion", ""}, {"targetVersion", "   "}, {"targetVersion", strings.Repeat("x", 4097)}, {"padded target", " target "},
		{"artifactURL", "https://artifact.invalid/image?token=secret"}, {"intervalSeconds", 0}, {"intervalSeconds", 86401},
		{"apiPoolSelector", map[string]string{}}, {"apiPoolReplicas", 0}, {"checkStore", false},
	} {
		t.Run(fmt.Sprint(tc.field, "-", tc.value), func(t *testing.T) {
			fields := map[string]any{
				"firmware":     map[string]any{"n": 1, "targetVersion": "target"},
				"observation":  map[string]any{"intervalSeconds": 30},
				"nativeHealth": map[string]any{"apiPoolSelector": map[string]string{"job": "api"}, "apiPoolReplicas": 1},
			}
			section := "firmware"
			switch tc.field {
			case "padded target":
				fields["firmware"].(map[string]any)["targetVersion"] = tc.value
			case "default cadence":
				fields["observation"].(map[string]any)["intervalSeconds"] = tc.value
			case "intervalSeconds":
				section = "observation"
			case "apiPoolSelector", "apiPoolReplicas":
				section = "nativeHealth"
			case "checkStore":
				fields[tc.field] = tc.value
			}
			valid := tc.field == "valid" || tc.field == "default cadence" || tc.field == "padded target"
			if !valid && tc.field != "checkStore" {
				fields[section].(map[string]any)[tc.field] = tc.value
			}
			raw, err := yaml.Marshal(fields)
			if err != nil {
				t.Fatal(err)
			}
			var n yaml.Node
			if err = yaml.Unmarshal(raw, &n); err != nil {
				t.Fatal(err)
			}
			parsed, err := ParseFirmwareSettings(n)
			if (err == nil) != valid || tc.field == "padded target" && parsed.Firmware.TargetVersion != "target" {
				t.Fatalf("settings: %v", err)
			}
		})
	}
}

// These assertions also guard sanitized transactional-setter classification.
func TestFirmwareRPC(t *testing.T) {
	for _, code := range []codes.Code{codes.Unauthenticated, codes.PermissionDenied, codes.InvalidArgument, codes.ResourceExhausted, codes.Unavailable, codes.DeadlineExceeded} {
		a := firmwareRPC(status.Error(code, "https://secret.example/token"))
		definite := code == codes.Unauthenticated || code == codes.PermissionDenied || code == codes.InvalidArgument
		if (a.Rejected != nil) != definite || a.NoReply == definite || strings.Contains(fmt.Sprint(a), "secret") {
			t.Fatal(code, a)
		}
	}
}

func TestFirmwareEvidenceQuota(t *testing.T) {
	for _, limit := range []int{128 << 20, 256 << 20} {
		t.Run(fmt.Sprint(limit), func(t *testing.T) {
			e, err := openEvidence(t.TempDir(), "evidence", limit)
			if err != nil {
				t.Fatal(err)
			}
			defer func() { _ = e.file.Close() }()
			e.bytes = limit - 1
			if err = e.append("change"); err == nil {
				t.Fatal("quota bypassed")
			}
			stat, err := e.file.Stat()
			if err != nil || stat.Size() != 0 {
				t.Fatal("over-quota append reached disk", err)
			}
		})
	}
}

func TestFirmwareVerification(t *testing.T) {
	for _, mode := range []string{"heartbeat", "probe 5 manager 30 poll 15", "manager gap", "lock loser first", "pre-completion increments", "manager reset", "manager replaced"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				site.config.Metrics.ScrapeIntervalSeconds = 5
				test, r, ctl := firmwareHarness(t, site)
				test.cfg.Probe.PollSeconds = 15
				sp, _ := test.deps.Store(context.Background())
				m := sp.Client.(*firmwareMetrics)
				m.mode = mode
				m.completed = func() time.Time { return test.wave.completed }
				if mode == "lock loser first" {
					test.deps.Pods.(*fakePods).pods["core"] = append(test.deps.Pods.(*fakePods).pods["core"], cluster.PodInfo{Name: "api-1", UID: "uid-3", Ready: true})
				}
				if mode == "manager replaced" {
					m.afterManager = func(n int) {
						if n == 3 {
							test.deps.Pods.(*fakePods).set("core", "replacement")
						}
					}
				}
				out, err := firmwareRunTest(test, r, ctl, false)
				valid := mode == "probe 5 manager 30 poll 15" || mode == "lock loser first"
				if (out.Verdict == result.Passed) != valid || valid && err != nil || site.writes != 1 || !r.j.Unsettled() {
					t.Fatalf("verification: %v %v", out, err)
				}
			})
		})
	}
}

// Keep parent deadline delivery pending while its child probe deadline fires.
type firmwarePendingDeadline struct {
	context.Context
	until time.Time
}

func (c firmwarePendingDeadline) Deadline() (time.Time, bool) { return c.until, true }

func TestFirmwareCleanup(t *testing.T) {
	for _, mode := range []string{"valid", "foreign bounds", "lost Lease", "soft stop", "hard cancel", "active request", "unknown active work", "already closed", "pool unhealthy", "API replaced", "probe restarted", "probe restarted closed", "foreign instance", "foreign window elsewhere", "foreign request", "changed target", "stale evidence", "changed identity", "journal failure", "deadline", "census unavailable", "early evidence", "stop mid sweep", "probe starting", "probe offline", "config reader unavailable", "late owned request", "cancel after clean", "epoch during read", "epoch during family", "census recovers", "lost Lease during outage", "lost Lease during census", "host read deadline", "census read deadline", "cancel cleanup read"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				test, r, ctl := firmwareHarness(t, site)
				if mode == "soft stop" || mode == "early evidence" || mode == "stop mid sweep" {
					site.action = ctl.RequestStop
				}
				if mode == "hard cancel" {
					site.action = ctl.Cancel
				}
				if mode == "stop mid sweep" {
					sp, _ := test.deps.Store(context.Background())
					sp.Client.(*firmwareMetrics).baselineDelay = 20 * time.Second
				}
				out, runErr := firmwareRunTest(test, r, ctl, false)
				if mode == "soft stop" || mode == "hard cancel" || mode == "early evidence" || mode == "stop mid sweep" {
					if out.Verdict == result.Passed || runErr == nil || site.writes != 1 || !r.j.Unsettled() {
						t.Fatalf("stop not exercised: %v %v", out, runErr)
					}
				} else if out.Verdict != result.Passed || runErr != nil {
					t.Fatalf("run=%v %v", out, runErr)
				}
				lostAt := time.Time{}
				switch mode {
				case "lost Lease during outage":
					site.outageUntil = time.Now().Add(time.Hour)
					r.gate.lost.Store(true)
					lostAt = time.Now()
				case "lost Lease during census":
					site.requestOutageUntil = time.Now().Add(time.Hour)
					site.onCensusFailure = func() { r.gate.lost.Store(true); lostAt = time.Now() }
				case "probe starting", "probe offline":
					site.outageUntil = time.Now().Add(20 * time.Second)
					site.configOutage = mode == "probe offline"
					site.process++
					site.config.Metrics.ProcessStartSeconds = site.process
				case "config reader unavailable":
					site.config.NicoAPI.LastAttemptError = new("reader unavailable")
				case "late owned request":
					proofAt := time.Time{}
					site.onUnits = func(family probeapi.Family) {
						if family != probeapi.FamilyHostFirmwareRequests || site.writes < 2 {
							return
						}
						if proofAt.IsZero() {
							proofAt = site.meta(probeapi.FamilyHost).ReadAt.Time
						}
						if site.meta(family).SweepStart.After(proofAt) {
							site.state[family]["h0"] = probeapi.Unit{ID: "h0", Type: probeapi.UnitTypeHostFirmwareRequest, Source: probeapi.SourceNicoAPI}
						}
					}
				case "epoch during family":
					site.onUnits = func(family probeapi.Family) {
						if family == probeapi.FamilyDpu {
							site.onUnits = nil
							site.process++
							site.config.Metrics.ProcessStartSeconds = site.process
						}
					}
				case "epoch during read":
					reads := 0
					site.onConfig = func() {
						reads++
						if reads == 2 {
							site.process++
							site.config.Metrics.ProcessStartSeconds = site.process
						}
					}
				case "census recovers":
					site.requestOutageUntil = time.Now().Add(80 * time.Second)
				case "pool unhealthy":
					sp, _ := test.deps.Store(context.Background())
					sp.Client.(*firmwareMetrics).unhealthy.Store(true)
				case "API replaced":
					test.deps.Pods.(*fakePods).set("core", "replacement")
				case "probe restarted", "probe restarted closed":
					if mode == "probe restarted closed" {
						site.snapshotEpoch = site.process
						site.process += .0005 // A new epoch in the same millisecond ID bucket.
					} else {
						site.process++
					}
					site.config.Metrics.ProcessStartSeconds = site.process
					if mode == "probe restarted closed" {
						site.state[probeapi.FamilyHost]["h0"].Host.FirmwareWindow = &probeapi.FirmwareWindow{}
					}
				case "foreign instance":
					site.state[probeapi.FamilyInstance]["foreign"] = probeapi.Unit{ID: "foreign", Type: probeapi.UnitTypeInstance, Source: probeapi.SourceNicoAPI}
				case "foreign window elsewhere":
					u := site.state[probeapi.FamilyHost]["h0"]
					raw, _ := json.Marshal(u)
					var foreign probeapi.Unit
					if e := json.Unmarshal(raw, &foreign); e != nil {
						t.Fatal(e)
					}
					foreign.ID = "outside"
					foreign.Host.FirmwareWindow.End.Time = foreign.Host.FirmwareWindow.End.Add(time.Second)
					site.state[probeapi.FamilyHost][foreign.ID] = foreign
				case "foreign request":
					site.state[probeapi.FamilyHostFirmwareRequests]["outside"] = probeapi.Unit{ID: "outside", Type: probeapi.UnitTypeHostFirmwareRequest, Source: probeapi.SourceNicoAPI}
				case "changed target":
					site.config.NicoAPI.Revision = new(uint64(8))
				case "stale evidence", "early evidence":
					site.mode = "early snapshot"
				case "changed identity":
					u := site.state[probeapi.FamilyHost]["h0"]
					u.BmcIP = new("192.0.2.99")
					site.state[probeapi.FamilyHost]["h0"] = u
				case "journal failure":
					r.j.Inject(func(_ *os.File, _ []byte) error { return errors.New("fsync failure") })
				case "host read deadline", "census read deadline":
					site.mode = mode
				case "cancel cleanup read":
					site.mode = "host read deadline"
				case "census unavailable":
					site.mode = mode
				case "deadline":
					site.mode = "missing"
				case "foreign bounds":
					site.state[probeapi.FamilyHost]["h0"].Host.FirmwareWindow.End.Time = site.state[probeapi.FamilyHost]["h0"].Host.FirmwareWindow.End.Add(time.Second)
				case "lost Lease":
					r.gate.lost.Store(true)
				case "active request":
					site.state[probeapi.FamilyHostFirmwareRequests]["h0"] = probeapi.Unit{ID: "h0", Type: probeapi.UnitTypeHostFirmwareRequest, Source: probeapi.SourceNicoAPI, Stage: probeapi.StageRegistered, State: "Maintenance", Host: &probeapi.HostFields{}}
				case "unknown active work":
					site.state[probeapi.FamilyHost]["h0"].Host.Firmware.WaitingForRefresh = new(true)
				case "already closed":
					site.state[probeapi.FamilyHost]["h0"].Host.FirmwareWindow = &probeapi.FirmwareWindow{}
				}
				if !slices.Contains([]string{"probe restarted closed", "early evidence", "stop mid sweep", "probe starting", "probe offline", "lost Lease during outage"}, mode) {
					time.Sleep(30 * time.Second)
				} // Publish the changed immutable frame.
				budget := 3 * time.Minute
				if mode == "deadline" {
					budget = 10 * time.Second
				}
				ctx, cancel := context.WithTimeout(context.Background(), budget)
				if strings.HasSuffix(mode, "read deadline") || mode == "cancel cleanup read" {
					cancel()
					ctx, cancel = context.WithCancel(context.Background())
					ctx = firmwarePendingDeadline{ctx, time.Now().Add(budget)}
				}
				defer cancel()
				if mode == "cancel cleanup read" {
					site.onConfig = func() { time.Sleep(budget); cancel() }
				}
				if mode == "cancel after clean" {
					test.dial = func(string) (firmwareWriter, error) { return firmwareHook{firmwareWriter: site, closed: cancel}, nil }
				}
				w := writer.NewFirmwareCleanup(r.j, r.gate, time.Now().Add(3*time.Minute))
				defer w.Close()
				start := time.Now()
				_, err := test.CloseFirmwareWindows(ctx, w)
				if strings.HasPrefix(mode, "lost Lease during") && (!errors.Is(err, writer.ErrLockLost) || time.Since(start) >= budget || time.Since(lostAt) != 0) {
					t.Fatalf("lost Lease hidden by retry: %v after %v", err, time.Since(start))
				}
				if mode == "lost Lease during census" && site.censusFailures == 0 {
					t.Fatal("census outage not exercised")
				}
				clean := slices.Contains([]string{"valid", "soft stop", "hard cancel", "already closed", "pool unhealthy", "API replaced", "probe restarted", "probe restarted closed", "foreign instance", "foreign window elsewhere", "foreign request", "changed target", "stop mid sweep", "probe starting", "probe offline", "config reader unavailable", "cancel after clean", "epoch during read", "epoch during family", "census recovers"}, mode)
				if mode == "census recovers" && site.censusFailures == 0 {
					t.Fatal("census failure not exercised")
				}
				if mode == "census unavailable" && site.writes != 2 {
					t.Fatal("unknown census blocked closure")
				}
				if (mode == "deadline" || mode == "census unavailable" || strings.HasSuffix(mode, "read deadline")) && !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("deadline cause lost: %v", err)
				}
				if (strings.HasSuffix(mode, "read deadline") || mode == "cancel cleanup read") && site.deadlineReads != 1 {
					t.Fatal("deadline did not land in a probe read", site.deadlineReads)
				}
				var probeCause *probe.Error
				if strings.HasSuffix(mode, "read deadline") && !errors.As(err, &probeCause) {
					t.Fatal("original probe cause lost", err)
				}
				if mode == "cancel cleanup read" && (!errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded)) {
					t.Fatal("real cancellation misclassified", err)
				}
				if (err == nil) != clean || r.j.Unsettled() == clean {
					t.Fatalf("cleanup=%v pending=%v", err, r.j.Unsettled())
				}
				noClose := slices.Contains([]string{"lost Lease", "foreign bounds", "already closed", "probe restarted closed", "stale evidence", "changed identity", "journal failure", "early evidence", "lost Lease during outage", "host read deadline", "cancel cleanup read"}, mode)
				if (noClose && site.writes != 1) || (clean && !noClose && site.writes != 2) {
					t.Fatalf("unexpected close writes=%d", site.writes)
				}
			})
		})
	}
}

type firmwareHook struct {
	firmwareWriter
	after  func(context.Context)
	closed func()
}

func (h firmwareHook) Close() error {
	err := h.firmwareWriter.Close()
	if h.closed != nil {
		h.closed()
	}
	return err
}

func (h firmwareHook) SetFirmwareUpdateTimeWindow(ctx context.Context, r *corev1.SetFirmwareUpdateTimeWindowRequest) error {
	err := h.firmwareWriter.SetFirmwareUpdateTimeWindow(ctx, r)
	if h.after != nil {
		h.after(ctx)
	}
	return err
}
func TestFirmwarePacedWaveAndSafety(t *testing.T) {
	for _, mode := range []string{"paced wave", "pool failure", "soft stop", "hard cancel", "Lease loss"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(101)
				test, r, ctl := firmwareHarness(t, site)
				r.w = writer.New(r.j, r.gate, ctl.Stopper(), time.Time{})
				sp, _ := test.deps.Store(context.Background())
				metrics := sp.Client.(*firmwareMetrics)
				test.dial = func(string) (firmwareWriter, error) {
					return firmwareHook{firmwareWriter: site, after: func(ctx context.Context) {
						if site.writes != 1 {
							return
						}
						switch mode {
						case "pool failure":
							metrics.unhealthy.Store(true)
							time.Sleep(20 * time.Second)
							if ctx.Err() != nil {
								t.Error("health stop cancelled an in-flight RPC")
							}
						case "soft stop":
							ctl.RequestStop()
						case "hard cancel":
							ctl.Cancel()
						case "Lease loss":
							r.gate.lost.Store(true)
						}
					}}, nil
				}
				out, err := firmwareRunTest(test, r, ctl, false)
				want := 1
				if mode == "paced wave" {
					want = 2
					if !slices.Equal(site.sizes, []int{100, 1}) || site.sent[1].Sub(site.sent[0]) < 500*time.Millisecond {
						t.Fatal("invalid firmware batch size or rate", site.sizes, site.sent)
					}
				}
				if site.writes != want || (out.Verdict == result.Passed) != (mode == "paced wave") {
					t.Fatalf("writes=%d verdict=%s error=%v reasons=%v", site.writes, out.Verdict, err, out.Reasons)
				}
			})
		})
	}
}

func TestFirmwareSweepDurationAdmission(t *testing.T) {
	for _, d := range []time.Duration{20 * time.Second, 57 * time.Second} {
		t.Run(d.String(), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				site.sweepDuration = d
				test, r, ctl := firmwareHarness(t, site)
				budget := time.Duration(firmwareCleanupMinimum(1, 30, 5)) * time.Second
				test.s.Firmware.CleanupSeconds = uint64(budget / time.Second)
				out, err := firmwareRunTest(test, r, ctl, false)
				if d > 22500*time.Millisecond {
					if err != nil || out.Verdict != result.NotReady || site.writes != 0 || len(site.catalogRequests) != 0 || !strings.Contains(strings.Join(out.Reasons, " "), "probe sweep duration exceeds") {
						t.Fatalf("duration refusal: verdict=%s writes=%d catalog=%d err=%v reasons=%v", out.Verdict, site.writes, len(site.catalogRequests), err, out.Reasons)
					}
					return
				}
				if err != nil || out.Verdict != result.Passed {
					t.Fatalf("supported sweep: %s %v %v", out.Verdict, err, out.Reasons)
				}
				ctx, cancel := context.WithTimeout(context.Background(), budget)
				defer cancel()
				w := writer.NewFirmwareCleanup(r.j, r.gate, time.Now().Add(budget))
				defer w.Close()
				if _, err := test.CloseFirmwareWindows(ctx, w); err != nil || r.j.Unsettled() || site.writes != 2 {
					t.Fatalf("cleanup=%v pending=%t writes=%d", err, r.j.Unsettled(), site.writes)
				}
			})
		})
	}
}

func TestFirmwareSlowCloseAtCleanupMinimum(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1000)
		test, r, ctl := firmwareHarness(t, site)
		test.cfg.Probe.PollSeconds = 15
		budget := firmwareCleanupMinimum(1000, 30, 15)
		test.s.Firmware.CleanupSeconds = budget
		if out, err := firmwareRunTest(test, r, ctl, false); err != nil || out.Verdict != result.Passed || site.writes != 10 {
			t.Fatal("opening failed", out, err, site.writes)
		}
		test.dial = func(string) (firmwareWriter, error) {
			return firmwareHook{firmwareWriter: site, after: func(ctx context.Context) {
				select {
				case <-time.After(30 * time.Second):
				case <-ctx.Done():
				}
			}}, nil
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Duration(budget)*time.Second)
		defer cancel()
		w := writer.NewFirmwareCleanup(r.j, r.gate, time.Now().Add(time.Duration(budget)*time.Second))
		defer w.Close()
		if _, err := test.CloseFirmwareWindows(ctx, w); err != nil || r.j.Unsettled() || site.writes != 20 {
			t.Fatalf("slow close: %v pending=%t writes=%d budget=%d", err, r.j.Unsettled(), site.writes, budget)
		}
	})
}

func TestFirmwareCleanupPollAdmission(t *testing.T) {
	for _, poll := range []uint64{59, 60, 61} {
		t.Run(fmt.Sprint(poll), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				test, r, ctl := firmwareHarness(t, site)
				test.cfg.Probe.PollSeconds = poll
				test.s.Firmware.CleanupSeconds = firmwareCleanupMinimum(1, 30, poll)
				out, err := firmwareRunTest(test, r, ctl, true)
				if poll < 60 {
					if err != nil || out.Verdict != result.Passed {
						t.Fatal(out, err)
					}
				} else if err != nil || out.Verdict != result.NotReady || !strings.Contains(strings.Join(out.Reasons, " "), "probe poll must be shorter than the firmware ownership grace") {
					t.Fatal("unsafe poll admitted or misclassified", out, err)
				}
				if site.writes != 0 || len(site.catalogRequests) != 0 {
					t.Fatal("poll admission sent a mutation")
				}
			})
		})
	}
}

func TestFirmwareCancellationAtCleanupMinimum(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		test, r, ctl := firmwareHarness(t, site)
		site.action = func() {
			if site.writes == 1 {
				site.sweepDuration, site.sweepInterval = 360*time.Second, 480*time.Second
				test.s.Observation.IntervalSeconds = 480
				ctl.Cancel()
			}
		}
		if out, err := firmwareRunTest(test, r, ctl, false); err == nil || out.Verdict == result.Passed || site.writes != 1 {
			t.Fatal("cancellation did not interrupt opening", out, err, site.writes)
		}
		test.cfg.Probe.PollSeconds = 15
		budget := time.Duration(firmwareCleanupMinimum(1, 480, 15)) * time.Second
		ctx, cancel := context.WithTimeout(context.Background(), budget)
		defer cancel()
		w := writer.NewFirmwareCleanup(r.j, r.gate, time.Now().Add(budget))
		defer w.Close()
		if _, err := test.CloseFirmwareWindows(ctx, w); err != nil || r.j.Unsettled() || site.writes != 2 {
			t.Fatalf("cleanup=%v pending=%t writes=%d budget=%s", err, r.j.Unsettled(), site.writes, budget)
		}
	})
}

func TestFirmwareUnknownNeverCompletes(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		site.mode = "no-upgrade"
		test, r, ctl := firmwareHarness(t, site)
		site.onUnits = func(family probeapi.Family) {
			if family == probeapi.FamilyHost && site.writes > 0 {
				u := site.state[family]["h0"]
				u.Host.ReportAt = new(site.meta(family).ReadAt)
				u.Host.Firmware.ReportVersion = new("after")
				u.Host.Firmware.Components[0].Phase = probeapi.FirmwarePhaseUnknown
				site.state[family]["h0"] = u
			}
		}
		out, err := firmwareRunTest(test, r, ctl, false)
		if err != nil || out.Verdict != result.Unresolved || len(test.wave.done) != 0 || !test.wave.completed.IsZero() || !r.j.Unsettled() || site.writes != 1 {
			t.Fatalf("Unknown completed: %s err=%v done=%d completed=%v pending=%t writes=%d", out.Verdict, err, len(test.wave.done), test.wave.completed, r.j.Unsettled(), site.writes)
		}
		if reasons := strings.Join(out.Reasons, " "); !strings.Contains(reasons, "deadline") || strings.Contains(reasons, "cancelled") || strings.Contains(reasons, "soft stop") {
			t.Fatalf("deadline reason: %s", reasons)
		}
	})
}

func TestFirmwareDeadlineCauses(t *testing.T) {
	for _, mode := range []string{"probe cancelled", "hard dial", "catalog reason", "catalog cancelled"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(2)
				test, r, ctl := firmwareHarness(t, site)
				hard := errors.New("hard dial failure")
				switch mode {
				case "probe cancelled":
					check := test.check
					test.check = func(ctx context.Context, d readiness.Deps, end time.Time) readiness.Report {
						time.Sleep(80 * time.Second)
						return check(ctx, d, end)
					}
				case "hard dial":
					test.dial = func(string) (firmwareWriter, error) {
						time.Sleep(80 * time.Second)
						return nil, errors.Join(hard, &probe.Error{Kind: probe.Cancelled})
					}
				case "catalog reason", "catalog cancelled":
					site.catalogMode = "absent"
					if mode == "catalog cancelled" {
						site.catalogMode = "read deadline"
						site.catalogConfigError = &probe.Error{Kind: probe.Cancelled}
					}
					site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
				}
				now := time.Now()
				out, err := test.Run(run.RunCtx{Ctx: ctl.Context(), Clock: run.RealClock{}, Deadline: now.Add(80 * time.Second), ProbeReadEnd: now.Add(time.Hour)}, r.w)
				reasons := strings.Join(out.Reasons, " ")
				if mode == "hard dial" {
					if !errors.Is(err, hard) {
						t.Fatalf("hard cause lost: %v %v", err, out.Reasons)
					}
				} else if err != nil || mode == "probe cancelled" && (!strings.Contains(reasons, "deadline") || strings.Contains(reasons, "cancelled")) || strings.HasPrefix(mode, "catalog") && (!strings.Contains(reasons, "reconcile before selecting a different target") || len(site.catalogRequests) != 1) {
					t.Fatalf("deadline cause: %v %v", err, out.Reasons)
				}
				if site.writes != 0 {
					t.Fatal("window opened after deadline")
				}
			})
		})
	}
}

func TestFirmwareReadFreshness(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		for _, tc := range []struct {
			name          string
			age, duration time.Duration
			want          bool
		}{
			{"300s sweep", 0, 300 * time.Second, false},
			{"duration boundary", 0, 90 * time.Second, true},
			{"duration exceeded", 0, 90*time.Second + time.Nanosecond, false},
			{"end boundary", 240 * time.Second, 90 * time.Second, true},
			{"stale end", 241 * time.Second, 90 * time.Second, false},
			{"future end", -time.Second, 90 * time.Second, false},
			{"inverted", 0, -time.Second, false},
			{"instant", 0, 0, true},
		} {
			end := time.Now().Add(-tc.age)
			read := probeapi.ReadInfo{ReadStart: jsonx.Time{Time: end.Add(-tc.duration)}, ReadAt: jsonx.Time{Time: end}, ReadIntervalSeconds: 120}
			if firmwareReadFresh(read, 120) != tc.want {
				t.Errorf("%s: completed-read freshness disagrees", tc.name)
			}
			site.config.NicoAPI.ReadInfo = read
			site.config.Metrics.ReaderIntervalsSeconds[probeapi.FamilyConfigNicoAPI] = 120
			if err := firmwareConfig(site.config, 120); (err == nil) != tc.want || tc.duration > 90*time.Second && !errors.Is(err, errFirmwareSweepDuration) {
				t.Errorf("%s: config freshness: %v", tc.name, err)
			}
			meta := site.meta(probeapi.FamilyHost)
			meta.SweepStart, meta.ReadAt, meta.SweepIntervalSeconds = read.ReadStart, read.ReadAt, 120
			if (completedSourceMeta(meta, probeapi.FamilyHost, site.process, 120) == nil) != tc.want {
				t.Errorf("%s: completed-family freshness disagrees", tc.name)
			}
			if tc.want && tc.age+tc.duration > 240*time.Second && sourceMeta(meta, probeapi.FamilyHost, site.process, 120) == nil {
				t.Fatal("instance start-age bound lost")
			}
		}
	})
}

type firmwarePinnedProbe struct {
	*firmwareSite
	mode string
}

func (p firmwarePinnedProbe) Units(ctx context.Context, filter probeapi.Filter, snapshot *string, page probe.Page) (probeapi.UnitsResponse, error) {
	r, err := p.firmwareSite.Units(ctx, filter, snapshot, page)
	if p.mode == "long sweep" {
		r.SweepStart.Time = r.ReadAt.Add(-300 * time.Second)
	}
	if p.mode == "duration boundary" {
		r.SweepStart.Time = r.ReadAt.Add(-22500 * time.Millisecond)
	}
	if r.Family == probeapi.FamilyHost {
		r.FirmwareConfig.ReadStart.Time = r.SweepStart.Add(100 * time.Millisecond)
		r.FirmwareConfig.ReadAt.Time = r.SweepStart.Add(200 * time.Millisecond)
		switch p.mode {
		case "revision":
			r.FirmwareConfig.Revision++
		case "future":
			r.FirmwareConfig.ReadAt.Time = r.ReadAt.Add(time.Nanosecond)
		case "inverted":
			r.FirmwareConfig.ReadStart.Time = r.FirmwareConfig.ReadAt.Add(time.Second)
		}
	}
	return r, err
}
func TestFirmwareConfigRefreshDuringSweep(t *testing.T) {
	for _, mode := range []string{"same revision", "duration boundary", "long sweep", "revision", "future", "inverted"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				p := firmwarePinnedProbe{firmwareFixture(1), mode}
				first, err := readFirmware(context.Background(), p, time.Now().Add(time.Minute), 30, false)
				if (err == nil) != (mode == "same revision" || mode == "duration boundary") {
					t.Fatalf("config provenance: %v", err)
				}
				if mode == "duration boundary" {
					next, e := readFirmware(context.Background(), p, time.Now().Add(time.Minute), 30, false, first)
					if e != nil || next.Rows[probeapi.FamilyHost]["h0"].Host != first.Rows[probeapi.FamilyHost]["h0"].Host {
						t.Fatalf("long sweep cache not reused: %v", e)
					}

				}
			})
		})
	}
}
func TestFirmwareBaselineReadCause(t *testing.T) {
	for _, tc := range []struct {
		mode, reason string
	}{
		{"baseline timeout", "context deadline exceeded"},
		{"baseline canceled", "context canceled"},
		{"baseline mismatch", "stored firmware baseline is missing or disagrees"},
	} {
		t.Run(tc.mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				test, r, ctl := firmwareHarness(t, firmwareFixture(1))
				test.deps.Store = func(context.Context) (readiness.StoreProbe, error) {
					return readiness.StoreProbe{Client: &firmwareMetrics{mode: tc.mode}}, nil
				}
				out, err := firmwareRunTest(test, r, ctl, false)
				if err != nil || out.Verdict != result.NotReady || !slices.Contains(out.Reasons, tc.reason) {
					t.Fatalf("baseline: %s %v %v", out.Verdict, err, out.Reasons)
				}
			})
		})
	}
}

func TestFirmwareCutoffMidWave(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(101)
		test, r, ctl := firmwareHarness(t, site)
		site.action = func() { test.wave.cutoff = time.Now().Add(-time.Second) }
		out, err := firmwareRunTest(test, r, ctl, false)
		if out.Verdict != result.Unresolved || err != nil || site.writes != 1 || len(test.wave.attempts) != 1 {
			t.Fatalf("cutoff: %s %v writes=%d", out.Verdict, err, site.writes)
		}
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
		defer cancel()
		w := writer.NewFirmwareCleanup(r.j, r.gate, time.Now().Add(3*time.Minute))
		defer w.Close()
		if _, err = test.CloseFirmwareWindows(ctx, w); err != nil || site.writes != 2 || r.j.Unsettled() {
			t.Fatalf("cutoff cleanup: %v writes=%d", err, site.writes)
		}
	})
}

func TestFirmwareBackgroundTiming(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(2)
		test, r, ctl := firmwareHarness(t, site)
		test.s.Firmware.N = 1
		if out, err := firmwareRunTest(test, r, ctl, false); err != nil || out.Verdict != result.Passed {
			t.Fatalf("admission: %v %v", out, err)
		}
		site.state[probeapi.FamilyHost]["h1"].Host.Firmware.Components[0].Phase = probeapi.FirmwarePhaseCurrent
		time.Sleep(30 * time.Second)
		if err := test.wave.refresh(context.Background(), time.Now().Add(time.Minute)); err != nil || !test.wave.timingInvalid || test.wave.background != 1 {
			t.Fatalf("outside timing change missed: %v", err)
		}
	})
}

func TestFirmwareCatalog(t *testing.T) {
	for _, mode := range []string{"already target", "accepted", "lost", "absent", "pre-send proof", "conflict", "denied", "window denied", "dry", "false explicit", "unknown name", "duplicate", "empty catalog", "empty dry", "preflight shortfall", "post-write shortfall", "lost Lease", "unhealthy", "dry unhealthy", "already-target unhealthy", "unread catalog"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(2)
				test, r, ctl := firmwareHarness(t, site)
				site.catalogMode = mode
				if mode != "already target" && mode != "window denied" && mode != "already-target unhealthy" {
					site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
				}
				switch mode {
				case "window denied":
					site.mode = "rejected"
				case "false explicit":
					site.config.NicoAPI.FirmwareTargets[1].ExplicitStart = false
				case "unknown name":
					site.config.NicoAPI.FirmwareTargets[1].ComponentName = new("drive-unknown")
				case "duplicate":
					site.config.NicoAPI.FirmwareTargets = append(site.config.NicoAPI.FirmwareTargets, site.config.NicoAPI.FirmwareTargets[0])
				case "lost Lease":
					r.gate.lost.Store(true)
				case "unhealthy", "dry unhealthy", "already-target unhealthy":
					test.deps.Store = func(context.Context) (readiness.StoreProbe, error) {
						m := &firmwareMetrics{}
						m.unhealthy.Store(true)
						return readiness.StoreProbe{Client: m}, nil
					}
				case "preflight shortfall":
					site.state[probeapi.FamilyHost]["h0"].Host.Allocatable = new(false)
				case "post-write shortfall":
					for i := range site.state[probeapi.FamilyHost]["h0"].Host.Firmware.Components[0].Entries {
						site.state[probeapi.FamilyHost]["h0"].Host.Firmware.Components[0].Entries[i].Version = new("target")
					}
				case "empty catalog", "empty dry":
					site.runtimeOrdering = nil
					site.config.NicoAPI.FirmwareTargets = []probeapi.FirmwareTarget{}
					for _, u := range site.state[probeapi.FamilyHost] {
						u.Host.Firmware = nil
					}
				case "unread catalog":
					site.config.NicoAPI.LastAttemptError = new("source_unavailable")
				}
				started := time.Now()
				out, err := firmwareRunTest(test, r, ctl, mode == "dry" || mode == "empty dry" || mode == "dry unhealthy")
				success := slices.Contains([]string{"already target", "accepted", "lost", "dry", "empty catalog", "empty dry"}, mode)
				if (out.Verdict == result.Passed) != success || success && err != nil {
					t.Fatalf("verdict %s, error %v, reasons %v", out.Verdict, err, out.Reasons)
				}
				sent := slices.Contains([]string{"accepted", "lost", "absent", "pre-send proof", "conflict", "denied", "empty catalog", "post-write shortfall"}, mode)
				if len(site.catalogRequests) > 1 || (len(site.catalogRequests) == 1) != sent || !success && mode != "window denied" && site.writes != 0 || (mode == "dry" || mode == "empty dry") && site.writes != 0 {
					t.Fatalf("catalog=%d windows=%d", len(site.catalogRequests), site.writes)
				}
				var detail map[string]any
				if e := json.Unmarshal(out.Detail, &detail); e != nil {
					t.Fatal(e)
				}
				if detail["catalogRestorePolicy"] != "never" || detail["catalogTarget"] != "target" {
					t.Fatal(detail)
				}
				if (mode == "dry" || mode == "empty dry") && (detail["catalogWriteNeeded"] != true || detail["structuralEligibleCount"] != float64(2) || len(out.Stages) != 0) {
					t.Fatal("dry admission claims", detail, out.Stages)
				}
				if (mode == "denied" || mode == "window denied") && !slices.ContainsFunc(out.Reasons, func(reason string) bool {
					return strings.Contains(fmt.Sprint(reason), "firmware client not authorized")
				}) {
					t.Fatal("authorization cause lost", out.Reasons)
				}
				if mode == "window denied" && (site.writes != 1 || !closedFirmwareWindow(site.state[probeapi.FamilyHost]["h0"].Host.FirmwareWindow)) {
					t.Fatal("authorization rejection opened a window")
				}
				if mode == "lost" && r.j.Counts().Unclear != 0 {
					t.Fatal("catalog proof not settled", r.j.Counts())
				}
				if mode == "accepted" || mode == "lost" {
					b, e := os.ReadFile(r.j.Path())
					if e != nil {
						t.Fatal(e)
					}
					raw := string(b)
					plan, intent, receipt, proof := strings.Index(raw, `"entry":"firmwareCatalogPlan"`), strings.Index(raw, `"kind":"firmware-catalog-persistent"`), strings.Index(raw, `"entry":"firmwareCatalogReceipt"`), strings.Index(raw, `"entry":"firmwareCatalogProof"`)
					if plan < 0 || intent <= plan || receipt <= intent || proof <= receipt || strings.Contains(raw, test.s.Firmware.ArtifactURL) {
						t.Fatal("catalog journal order or artifact disclosure")
					}
				}
				if mode == "post-write shortfall" && (out.Verdict != result.NotReady || out.ExitCode != 2) {
					t.Fatal("admission classification", out.Verdict, out.ExitCode)
				}
				if mode == "unread catalog" && !strings.Contains(strings.Join(out.Reasons, " "), "firmware config revision, cadence or latest attempt unavailable") {
					t.Fatal("unread source not refused by runner", out.Reasons)
				}
				if mode == "absent" || mode == "pre-send proof" {
					if err != nil || strings.Contains(strings.Join(out.Reasons, " "), writer.ErrSoftStop.Error()) || !strings.Contains(strings.Join(out.Reasons, " "), "unresolved firmware catalog") {
						t.Fatal("catalog deadline classification", err, out.Reasons)
					}
					if time.Since(started) > 100*time.Second {
						t.Fatal("catalog read-back exceeded three reader cycles plus one poll", time.Since(started))
					}
					if r.j.Counts().Unclear != 1 || r.j.Unsettled() {
						t.Fatal("catalog must remain truthfully unclear without a window obligation", r.j.Counts())
					}
				}
			})
		})
	}
}

func TestFirmwareCatalogOrderingFallback(t *testing.T) {
	for _, mode := range []string{"static only", "runtime both", "add UEFI", "both rejected", "dry", "unclear first", "denied first"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(1)
				site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
				test, r, ctl := firmwareHarness(t, site)
				calls, passed := 1, true
				switch mode {
				case "static only", "dry":
					site.runtimeOrdering = nil
					calls = 2
					if mode == "dry" {
						calls = 0
					}
				case "add UEFI":
					site.runtimeOrdering = []corev1.HostFirmwareComponentType{1}
					test.s.Firmware.Component = probeapi.ComponentUefi
					site.config.NicoAPI.FirmwareTargets[0].Version = "target"
					groups := site.state[probeapi.FamilyHost]["h0"].Host.Firmware.Components
					groups[0], groups[1] = groups[1], groups[0]
					groups[0].Phase, groups[1].Phase = probeapi.FirmwarePhaseBehind, probeapi.FirmwarePhaseCurrent
				case "both rejected":
					site.runtimeOrdering = []corev1.HostFirmwareComponentType{4}
					site.config.NicoAPI.FirmwareTargets = site.config.NicoAPI.FirmwareTargets[1:]
					site.config.NicoAPI.FirmwareTargets = append(site.config.NicoAPI.FirmwareTargets, probeapi.FirmwareTarget{Vendor: "NVIDIA", Model: "GB200 NVL", Component: probeapi.ComponentOther, ComponentName: new("nic"), Version: "current", ExplicitStart: true})
					calls, passed = 2, false
				case "unclear first", "denied first":
					site.runtimeOrdering = nil
					site.catalogMode = map[string]string{"unclear first": "absent", "denied first": "denied"}[mode]
					passed = false
				}
				out, err := firmwareRunTest(test, r, ctl, mode == "dry")
				windows := 0
				if passed && mode != "dry" {
					windows = 1
				}
				if err != nil || (out.Verdict == result.Passed) != passed || !passed && out.Verdict != result.NotReady || len(site.catalogRequests) != calls || site.writes != windows {
					t.Fatalf("%s %v reasons=%v catalog=%d windows=%d", out.Verdict, err, out.Reasons, len(site.catalogRequests), site.writes)
				}
				if mode == "both rejected" && !strings.Contains(strings.Join(out.Reasons, " "), "set the runtime ordering manually") {
					t.Fatal("manual refusal lost", out.Reasons)
				}
				if calls == 2 {
					if !slices.Equal(orderingTypes(site.catalogRequests[1].ComponentOrdering), []corev1.HostFirmwareComponentType{1}) {
						t.Fatal("fallback scope", site.catalogRequests)
					}
					b, e := os.ReadFile(r.j.Path())
					if e != nil {
						t.Fatal(e)
					}
					intents := []journal.Intent{}
					for _, line := range strings.Split(strings.TrimSpace(string(b)), "\n") {
						var in journal.Intent
						if e := json.Unmarshal([]byte(line), &in); e != nil {
							t.Fatal(e)
						}
						if in.Entry == "intent" && in.Kind == "firmware-catalog-persistent" {
							intents = append(intents, in)
						}
					}
					if len(intents) != 2 || intents[0].Op == intents[1].Op || intents[0].Key == intents[1].Key || intents[0].RequestDigest == intents[1].RequestDigest || intents[0].Attempt != 1 || intents[1].Attempt != 1 {
						t.Fatal("fallback must be a new journaled operation", intents)
					}
					first := strings.Index(string(b), `"reason":"grpc-InvalidArgument"`)
					second := strings.Index(string(b), `"key":"`+intents[1].Key+`"`)
					if first < 0 || second <= first {
						t.Fatal("fallback preceded definite rejection")
					}
				}
			})
		})
	}
}

func TestFirmwareCatalogRequest(t *testing.T) {
	for _, component := range []probeapi.Component{probeapi.ComponentBmc, probeapi.ComponentUefi} {
		t.Run(string(component), func(t *testing.T) {
			site := firmwareFixture(1)
			test, _, _ := firmwareHarness(t, site)
			test.s.Firmware.Component = component
			test.s.Firmware.ArtifactURL = "https://artifact.invalid/image"
			test.s.Firmware.ArtifactSHA256 = strings.Repeat("ab", 32)
			body, needed, err := firmwareCatalogRequest(site.config, "NVIDIA", "GB200 NVL", test.s)
			if err != nil || needed != (component == probeapi.ComponentUefi) {
				t.Fatal(needed, err)
			}
			if body.Vendor != "NVIDIA" || body.Model != "GB200 NVL" || body.ExplicitStartNeeded == nil || !*body.ExplicitStartNeeded || len(body.Components) != 1 || !slices.Equal(orderingTypes(body.ComponentOrdering), []corev1.HostFirmwareComponentType{1, 3}) {
				t.Fatal(body)
			}
			selected := body.Components[0]
			expected := corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_BMC
			if component == probeapi.ComponentUefi {
				expected = corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_UEFI
			}
			if selected.Type != expected || len(selected.Firmware) != 1 {
				t.Fatal(selected)
			}
			v := selected.Firmware[0]
			if v.Version != "target" || !v.Default || !v.InstallOnlySpecified || len(v.Artifacts) != 1 || v.Artifacts[0].Url != test.s.Firmware.ArtifactURL || v.Artifacts[0].Sha256 == nil || *v.Artifacts[0].Sha256 != test.s.Firmware.ArtifactSHA256 {
				t.Fatal("catalog request changed its merge scope or artifact")
			}
		})
	}
}

func TestFirmwareCatalogReadFailures(t *testing.T) {
	for _, tc := range []struct {
		name         string
		family       bool
		failure      *probe.Error
		recoverAfter time.Duration
	}{
		{"config 403", false, &probe.Error{Kind: probe.Status, HTTPStatus: 403}, 0},
		{"units decode", true, &probe.Error{Kind: probe.Decode}, 0},
		{"transient expiry", false, &probe.Error{Kind: probe.Transport}, 0},
		{"read deadline", false, &probe.Error{Kind: probe.Cancelled}, 0},
		{"config recovers", false, &probe.Error{Kind: probe.Status, HTTPStatus: 503}, 20 * time.Second},
		{"units recover", true, &probe.Error{Kind: probe.Timeout}, 40 * time.Second},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(2)
				test, r, ctl := firmwareHarness(t, site)
				site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
				if out, err := firmwareRunTest(test, r, ctl, true); err != nil || out.Verdict != result.Passed {
					t.Fatal(out, err)
				}
				if tc.family {
					site.catalogUnitsError = tc.failure
				} else {
					site.catalogConfigError = tc.failure
				}
				site.catalogMode = tc.name
				site.catalogErrorFor = tc.recoverAfter
				err := test.wave.catalog(ctl.Context(), r.w, time.Now().Add(5*time.Minute), site.inv, false)
				if len(site.catalogRequests) != 1 || site.writes != 0 || site.catalogFailedReads == 0 {
					t.Fatal("unexpected mutations/reads", len(site.catalogRequests), site.writes, site.catalogFailedReads)
				}
				if tc.recoverAfter != 0 {
					if err != nil {
						t.Fatal("transient did not recover", err)
					}
					return
				}
				var pe *probe.Error
				if !errors.Is(err, tc.failure) || !errors.As(err, &pe) || pe != tc.failure {
					t.Fatal("original read cause lost", err)
				}
				if !tc.failure.Transient() && site.catalogFailedReads != 1 {
					t.Fatal("permanent read repeated", site.catalogFailedReads)
				}
				if tc.name == "read deadline" && !strings.Contains(err.Error(), "unresolved firmware catalog") {
					t.Fatal("deadline became cancellation", err)
				}
				if (tc.failure.Transient() || tc.name == "read deadline") && (!errors.Is(err, context.DeadlineExceeded) || errors.Is(err, writer.ErrSoftStop)) {
					t.Fatal("proof expiry lost deadline type", err)
				}
				if tc.failure.Transient() && (site.catalogFailedReads < 2 || !strings.Contains(err.Error(), "unresolved firmware catalog")) {
					t.Fatal("expiry lost diagnostic", err, site.catalogFailedReads)
				}
			})
		})
	}
}

func TestFirmwareCatalogCancelledWait(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		test, r, ctl := firmwareHarness(t, site)
		site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
		if out, err := firmwareRunTest(test, r, ctl, true); err != nil || out.Verdict != result.Passed {
			t.Fatal(out, err)
		}
		ctx, cancel := context.WithCancel(ctl.Context())
		defer cancel()
		site.onConfig = func() {
			if !site.catalogSent.IsZero() {
				time.AfterFunc(time.Second, cancel)
			}
		}
		err := test.wave.catalog(ctx, r.w, time.Now().Add(time.Minute), site.inv, false)
		if time.Since(site.catalogSent) != time.Second || !errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) || errors.Is(err, writer.ErrSoftStop) || len(site.catalogRequests) != 1 || site.writes != 0 {
			t.Fatal("cancel became a deadline or stop", err)
		}
	})
}

func TestFirmwareCatalogPinLag(t *testing.T) {
	for _, lag := range []time.Duration{0, 30 * time.Second, 45 * time.Second} {
		t.Run(lag.String(), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				site := firmwareFixture(2)
				test, r, ctl := firmwareHarness(t, site)
				site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
				site.catalogPinLag = lag
				test.cfg.Timeouts.ReadinessSeconds = 75 // README ratio: 2.5 reader intervals.
				out, err := firmwareRunTest(test, r, ctl, false)
				if err != nil || out.Verdict != result.Passed || len(site.catalogRequests) != 1 || site.writes != 1 {
					t.Fatalf("pin lag %v: %v %v %v", lag, out.Verdict, err, out.Reasons)
				}
			})
		})
	}
}

func TestFirmwareCatalogProofJournalDelay(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		test, r, ctl := firmwareHarness(t, site)
		site.catalogMode = "lost"
		site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
		delayed := false
		r.j.AfterSync = func(v any) {
			b, _ := json.Marshal(v)
			if strings.Contains(string(b), `"entry":"firmwareCatalogSettled"`) {
				delayed = true
				time.Sleep(70 * time.Second)
			}
		}
		out, err := firmwareRunTest(test, r, ctl, false)
		if !delayed || time.Since(site.catalogSent) <= 95*time.Second || err != nil || out.Verdict != result.Passed || len(site.catalogRequests) != 1 || site.writes != 1 {
			t.Fatal("proof persistence became a stop", out.Verdict, err, out.Reasons)
		}
	})
}

func TestFirmwareCatalogCutoff(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		site := firmwareFixture(1)
		test, r, ctl := firmwareHarness(t, site)
		site.config.NicoAPI.FirmwareTargets[0].Version = "previous"
		test.cfg.Timeouts.ReadinessSeconds = 2
		r.j.AfterSync = func(v any) {
			if in, ok := v.(journal.Intent); ok && in.Kind == "firmware-catalog-persistent" {
				time.Sleep(3 * time.Second)
			}
		}
		out, err := firmwareRunTest(test, r, ctl, false)
		if err != nil || out.Verdict != result.NotReady || out.ExitCode != 2 || len(site.catalogRequests) != 0 || !strings.Contains(strings.Join(out.Reasons, " "), writer.ErrCutoff.Error()) {
			t.Fatal("cutoff classification", out, err)
		}
	})
}

func TestFirmwareCatalogComponentNames(t *testing.T) {
	for name, want := range map[string]corev1.HostFirmwareComponentType{"hgxbmc": 7, "cpldmb": 5, "cpldpdb": 6, "combinedbmcuefi": 8, "cec": 2, "nic": 4, "gpu": 9, "cx7": 10} {
		t.Run(name, func(t *testing.T) {
			site := firmwareFixture(1)
			test, _, _ := firmwareHarness(t, site)
			site.config.NicoAPI.FirmwareTargets[1].ComponentName = new(name)
			site.config.NicoAPI.FirmwareTargets[1].Component = probeapi.ComponentOther
			body, _, err := firmwareCatalogRequest(site.config, "NVIDIA", "GB200 NVL", test.s)
			if err != nil || !slices.Equal(orderingTypes(body.ComponentOrdering), []corev1.HostFirmwareComponentType{1, want}) {
				t.Fatal("valid component rejected", name, err, body)
			}
		})
	}
}
