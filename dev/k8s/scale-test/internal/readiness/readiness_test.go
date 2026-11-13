// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness_test

import (
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/store"
)

// site fakes every seam but the BMCs, which are a real TLS fake.
type site struct {
	metrics           *probeapi.MetricsInfo
	configErr         error
	counts            map[string]uint64 // by count: "allocatable", "registered <unit type>" or "expected <record type>"
	countErrs         map[string]error
	stale             string // the count read from a stale sweep
	storeBuildErr     error
	reads             []probeapi.Family // the families the probe reads
	preflightErr      error
	status            store.Status // every family's, unless families holds its own
	families          map[probeapi.Family]store.Status
	statusErr         error
	stall             chan struct{} // FamilyStatus waits on it, ignoring its context
	pods              map[string][]cluster.PodInfo
	records           inventory.Response
	services          []cluster.MatService
	idErr, versionErr error
}

func (s *site) Config(context.Context, probe.ConfigFilter) (probeapi.ConfigResponse, error) {
	return probeapi.ConfigResponse{Metrics: s.metrics}, s.configErr
}
func (s *site) Count(_ context.Context, f probeapi.Filter) (probeapi.CountResponse, error) {
	name := "registered " + string(f.Type)
	switch {
	case f.Type == probeapi.UnitTypeHost && f.Allocatable != nil && *f.Allocatable:
		name = "allocatable"
	case f.Type == probeapi.UnitTypeExpectedRecord && f.RecordType != nil:
		name = "expected " + string(*f.RecordType)
	case f.Stage == nil || *f.Stage != probeapi.StageRegistered:
		return probeapi.CountResponse{}, errors.New("not a count fleet-up reads")
	}
	r := probeapi.CountResponse{Count: s.counts[name], SweepInfo: probeapi.SweepInfo{SweepAgeSeconds: 5, SweepIntervalSeconds: 10}}
	if name == s.stale {
		r.SweepAgeSeconds = 21
	}
	return r, s.countErrs[name]
}

// client pins the bounds of a 20 s scrape and 120 s readers with no ingestion delay: two intervals each.
func (s *site) client(context.Context) (readiness.StoreProbe, error) {
	sweep := 240 * time.Second
	return readiness.StoreProbe{Client: s, ScrapeBound: 40 * time.Second, SweepBounds: map[probeapi.Family]time.Duration{
		probeapi.FamilyDpu: sweep, probeapi.FamilyExpected: sweep, probeapi.FamilyHost: sweep, probeapi.FamilyRack: sweep}}, s.storeBuildErr
}
func (s *site) Preflight(_ context.Context, _ time.Time, required ...probeapi.Family) error {
	for _, f := range required {
		if !slices.Contains(s.reads, f) {
			return errors.New("the probe reads no " + string(f) + " sweeps")
		}
	}
	return s.preflightErr
}
func (s *site) FamilyStatus(_ context.Context, _ time.Time, f probeapi.Family) (store.Status, error) {
	if s.stall != nil {
		<-s.stall
	}
	if st, ok := s.families[f]; ok {
		return st, s.statusErr
	}
	return s.status, s.statusErr
}
func (s *site) ListPods(_ context.Context, namespace, _ string) ([]cluster.PodInfo, error) {
	return s.pods[namespace], nil
}
func (s *site) Read(context.Context, string) (inventory.Response, error) { return s.records, nil }
func (s *site) ListMatServices(context.Context) ([]cluster.MatService, error) {
	return s.services, nil
}

type mapDialer string

func (m mapDialer) Address(netip.Addr) string { return string(m) }

func services(machineType string, ips ...string) cluster.MatService {
	s := cluster.MatService{Name: machineType, MachineType: machineType}
	for _, ip := range ips {
		s.ExternalIPs = append(s.ExternalIPs, netip.MustParseAddr(ip))
	}
	return s
}

// ready is a ready site, its BMCs at addr: one rack, one host and its DPU, each BMC behind its own Service.
func ready(addr string) (*site, readiness.Deps) {
	s := &site{
		metrics: &probeapi.MetricsInfo{ContractVersion: probeapi.ContractVersion},
		reads:   []probeapi.Family{probeapi.FamilyExpected, probeapi.FamilyHost, probeapi.FamilyDpu, probeapi.FamilyRack},
		// The readers sweep every 120 s, so a sweep that ended 90 s ago is fresh.
		status: store.Status{SampleAgeSeconds: new(5.0), SweepCompleteAt: &jsonx.Time{Time: time.Now().Add(-90 * time.Second)}},
		pods: map[string][]cluster.PodInfo{"mat": {{Name: "mat-0", Ready: true, IP: "10.9.0.1"}},
			// A finished pod and Job pods serve nothing, so they are not Ready and do not count.
			"nico": {{Name: "nico-api-0", Ready: true, Restarts: 3}, {Name: "migrate", Phase: "Succeeded"},
				{Name: "job-a", Labels: map[string]string{"batch.kubernetes.io/job-name": "a"}}, {Name: "job-b", Labels: map[string]string{"job-name": "b"}}}},
		records: inventory.Response{CarbideAPIURL: "https://nico-api:1079", Racks: []inventory.RackIntent{{RackID: "r1"}},
			Devices: []inventory.Device{{Kind: inventory.KindMachine, BmcMac: "aa", RackID: new("r1")}},
			DPUs:    []inventory.DPU{{BmcMac: "bb", ParentBmcMac: "aa"}}},
		// A switch Service is not a device BMC.
		services: []cluster.MatService{services("host", "10.0.0.1"), services("dpu", "10.0.0.2"), services("switch", "10.0.0.3")},
	}
	return s, readiness.Deps{Probe: s, Store: s.client, Pods: s, Records: s, Services: s,
		Identity: func() (string, error) { return "id", s.idErr },
		Version:  func(context.Context, string) (string, error) { return "v1", s.versionErr },
		Dialer:   mapDialer(addr), Now: time.Now,
		Settings: readiness.Settings{Namespace: "mat", MatPort: 1266, Release: "v1", Expected: readiness.ExpectedCounts{Racks: 1, Hosts: 1, DPUs: 1},
			ControlPlane: []config.PodSelector{{Namespace: "nico", LabelSelector: "app=nico-api"}}},
	}
}

type fault struct {
	name   string
	fault  func(*site, *readiness.Deps)
	failed string // the one check that fails; "" is a ready site
	detail string // in that check's detail, or controlPlanePods' on a ready site
}

// TestRunFailsEachCheckForItsOwnFault proves every check passes on a ready site and fails, alone, for its own fault.
// Each Service fault fails although every BMC that is checked would answer.
func TestRunFailsEachCheckForItsOwnFault(t *testing.T) {
	bmcs := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Host != "10.0.0.1" && r.Host != "10.0.0.2" {
			http.NotFound(w, r)
			return
		}
		_, _ = io.WriteString(w, `{"@odata.id":"/redfish/v1"}`)
	}))
	defer bmcs.Close()
	faults := []fault{
		{"ready", func(*site, *readiness.Deps) {}, "", "1 serving pods Ready, restarts: nico/nico-api-0: 3 restarts"},
		{"probe down", func(s *site, _ *readiness.Deps) { s.configErr = &probe.Error{Kind: probe.Transport} }, "probe", "config read: probe: transport"},
		{"no metrics block", func(s *site, _ *readiness.Deps) { s.metrics = nil }, "probe", "declares no metrics store"},
		{"contract mismatch", func(s *site, _ *readiness.Deps) { s.metrics.ContractVersion = 2 }, "probe", "contract 2, the runner reads 1"},
		{"no allocatable read", func(s *site, _ *readiness.Deps) {
			s.countErrs = map[string]error{"allocatable": &probe.Error{Kind: probe.Unavailable}}
		}, "probe", "allocatable host count"},
		{"a count fails", func(s *site, _ *readiness.Deps) {
			s.countErrs = map[string]error{"expected host": &probe.Error{Kind: probe.Timeout}}
		}, "emptySite", "expected host: probe: timeout"},
		{"diagnostic run", func(_ *site, d *readiness.Deps) { d.Store = nil }, "metricsStore", "cannot admit ingestion"},
		{"store client error", func(s *site, _ *readiness.Deps) { s.storeBuildErr = errors.New("probe: noCompleteSweep") }, "metricsStore", "store client"},
		{"foreign probe process", func(s *site, _ *readiness.Deps) { s.preflightErr = errors.New("not the probe process") }, "metricsStore",
			"store preflight: not the probe process"},
		{"planned family not read", func(s *site, _ *readiness.Deps) { s.reads = s.reads[:1] }, "metricsStore", "store preflight: the probe reads no dpu sweeps"},
		{"store down", func(s *site, _ *readiness.Deps) { s.statusErr = errors.New("history unavailable") }, "metricsStore", "status read"},
		{"no scrape", func(s *site, _ *readiness.Deps) { s.status.SampleAgeSeconds = nil }, "metricsStore", "no dpu probe scrape in the last 40s"},
		{"stale scrape", func(s *site, _ *readiness.Deps) { s.status.SampleAgeSeconds = new(41.0) }, "metricsStore", "probe scrape in the last"},
		{"NaN scrape age", func(s *site, _ *readiness.Deps) { s.status.SampleAgeSeconds = new(math.NaN()) }, "metricsStore", "probe scrape in the last"},
		{"stale sweep end", func(s *site, _ *readiness.Deps) {
			s.status.SweepCompleteAt = &jsonx.Time{Time: time.Now().Add(-241 * time.Second)}
		}, "metricsStore", "no dpu sweep completed in the last 4m0s"},
		{"future sweep end", func(s *site, _ *readiness.Deps) {
			s.status.SweepCompleteAt = &jsonx.Time{Time: time.Now().Add(time.Minute)}
		}, "metricsStore", "sweep completed in the last"},
		{"stale DPU sweeps in the store", func(s *site, _ *readiness.Deps) {
			s.families = map[probeapi.Family]store.Status{probeapi.FamilyDpu: {SampleAgeSeconds: new(5.0), SweepCompleteAt: &jsonx.Time{Time: time.Now().Add(-time.Hour)}}}
		}, "metricsStore", "no dpu sweep completed in the last 4m0s"},
		{"no expected records in the store", func(s *site, _ *readiness.Deps) {
			s.families = map[probeapi.Family]store.Status{probeapi.FamilyExpected: {}}
		}, "metricsStore", "no expected probe scrape in the last 40s"},
		{"no racks in the store", func(s *site, _ *readiness.Deps) {
			s.families = map[probeapi.Family]store.Status{probeapi.FamilyRack: {}}
		}, "metricsStore", "no rack probe scrape in the last 40s"},
		{"count mismatch", func(_ *site, d *readiness.Deps) { d.Settings.Expected.Hosts = 2 }, "matInventory", "Hosts:1"},
		{"inventory problem", func(s *site, _ *readiness.Deps) { s.records.RegisterExpectedMachines = true }, "matInventory", "registers its own"},
		{"bad identity", func(s *site, _ *readiness.Deps) { s.idErr = errors.New("tls.crt: no such file") }, "nicoIdentity", "client certificate"},
		{"nico-api down", func(s *site, _ *readiness.Deps) { s.versionErr = errors.New("nico-api call failed") }, "nicoIdentity", "Version call"},
		{"other release", func(_ *site, d *readiness.Deps) { d.Settings.Release = "v2" }, "nicoIdentity", "other than expectedRelease"},
		{"no expected release", func(_ *site, d *readiness.Deps) { d.Settings.Release = "" }, "nicoIdentity", "expectedRelease is not set"},
		{"no control plane", func(_ *site, d *readiness.Deps) { d.Settings.ControlPlane = nil }, "controlPlanePods", "no control-plane selector"},
		{"only finished and Job pods", func(s *site, _ *readiness.Deps) { s.pods["nico"] = s.pods["nico"][1:] }, "controlPlanePods",
			"no serving pod matches app=nico-api in nico"},
		{"unready control plane", func(s *site, _ *readiness.Deps) { s.pods["nico"][0].Ready = false }, "controlPlanePods", "pod nico/nico-api-0 is not Ready"},
		{"only MAT pods", func(_ *site, d *readiness.Deps) { d.Settings.ControlPlane[0].Namespace = "mat" }, "controlPlanePods", "besides the MAT pods"},
		{"no BMC", func(s *site, d *readiness.Deps) {
			s.records.Devices, s.records.DPUs, s.services, d.Settings.Expected = nil, nil, s.services[2:], readiness.ExpectedCounts{Racks: 1}
		}, "bmc", "at least one"},
		{"missing Service", func(s *site, _ *readiness.Deps) { s.services = s.services[:1] }, "bmc", "found 0 distinct dpu BMC IPs, the inventory serves 1"},
		{"IP on two Services", func(s *site, _ *readiness.Deps) { s.services[1] = services("dpu", "10.0.0.1") }, "bmc", "BMC IP 10.0.0.1 is on 2 Services"},
		{"per-pod Service", func(s *site, _ *readiness.Deps) { s.services = append(s.services, services("pod-bmc", "10.0.0.4")) }, "bmc",
			"Service pod-bmc is per pod"},
		{"switch for a host", func(s *site, _ *readiness.Deps) { s.services[0].HardwareType = "switch" }, "bmc",
			"found 0 distinct host BMC IPs, the inventory serves 1; found 1 distinct host/switch BMC IPs, the inventory serves 0"},
		{"no external IP", func(s *site, _ *readiness.Deps) { s.services[0] = services("host") }, "bmc", "Service host has 0 external IPs, not 1"},
		{"two external IPs", func(s *site, _ *readiness.Deps) { s.services[0] = services("host", "10.0.0.1", "10.0.0.4") }, "bmc",
			"Service host has 2 external IPs, not 1"},
		{"BMCs fail", func(s *site, d *readiness.Deps) {
			for i := range 25 {
				ip := netip.AddrFrom4([4]byte{10, 0, 1, byte(i)}).String()
				s.records.Devices = append(s.records.Devices, inventory.Device{Kind: inventory.KindMachine, BmcMac: ip, RackID: new("r1")})
				s.services = append(s.services, services("host", ip))
			}
			d.Settings.Expected.Hosts = 26
		}, "bmc", "25 of 27 BMCs failed: 10.0.1.0: status 404; 10.0.1.1: status 404"},
	}
	// Every native count must be zero, from a fresh sweep.
	for _, name := range []string{"registered instance", "registered host", "registered dpu", "registered switch", "registered power_shelf",
		"registered rack", "expected host", "expected switch", "expected power_shelf", "expected rack", "expected rack_group"} {
		faults = append(faults, fault{"one " + name, func(s *site, _ *readiness.Deps) { s.counts = map[string]uint64{name: 1} }, "emptySite", name + ": 1 present"},
			fault{"stale " + name, func(s *site, _ *readiness.Deps) { s.stale = name }, "emptySite", name + ": no fresh sweep"})
	}
	for _, tc := range faults {
		t.Run(tc.name, func(t *testing.T) {
			s, d := ready(bmcs.Listener.Addr().String())
			tc.fault(s, &d)
			r := readiness.Run(context.Background(), d, time.Now().Add(10*time.Second))
			if r.Ready() != (tc.failed == "") {
				t.Errorf("ready = %v, report %+v", r.Ready(), r.Checks)
			}
			var names []string
			for _, c := range r.Checks {
				names = append(names, c.Name)
				if c.OK != (c.Name != tc.failed) {
					t.Errorf("%s ok = %v (%s), want only %q failed", c.Name, c.OK, c.Detail, tc.failed)
				}
				if c.Name == cmp.Or(tc.failed, "controlPlanePods") && !strings.Contains(c.Detail, tc.detail) {
					t.Errorf("%s detail = %q, want %q", c.Name, c.Detail, tc.detail)
				}
			}
			if got := strings.Join(names, ","); got != "bmc,controlPlanePods,emptySite,matInventory,metricsStore,nicoIdentity,probe" {
				t.Errorf("checks = %s, want all seven sorted by name", got)
			}
		})
	}
}

// TestRunFailsACheckStillRunningAtTheDeadline proves a check that ignores its context cannot hold the report past the
// deadline: it is failed as not finished, and the report says so in its findings.
func TestRunFailsACheckStillRunningAtTheDeadline(t *testing.T) {
	s, d := ready("127.0.0.1:1")
	s.stall = make(chan struct{})
	defer close(s.stall)
	start := time.Now()
	r := readiness.Run(context.Background(), d, start.Add(300*time.Millisecond))
	if time.Since(start) > 3*time.Second {
		t.Fatalf("Run took %v past a 300 ms deadline", time.Since(start))
	}
	var detail struct {
		Ready  bool
		Checks []readiness.Check
	}
	if err := json.Unmarshal(r.Detail(), &detail); err != nil || detail.Ready || len(detail.Checks) != 7 {
		t.Fatalf("detail = %s (%v)", r.Detail(), err)
	}
	if c := detail.Checks[4]; c.Name != "metricsStore" || c.OK || c.Detail != "not finished before the readiness deadline" {
		t.Errorf("stalled check = %+v, want metricsStore not finished", c)
	}
}

// TestRunTakesNoAnswerPastTheDeadline proves an answer the run clock sees at or past the deadline is not taken, even a
// pass while the context still runs: every check is not finished and the site is not ready.
func TestRunTakesNoAnswerPastTheDeadline(t *testing.T) {
	_, d := ready("127.0.0.1:1")
	deadline := time.Now().Add(10 * time.Second)
	d.Now = func() time.Time { return deadline }
	r := readiness.Run(context.Background(), d, deadline)
	for _, c := range r.Checks {
		if c.OK || c.Detail != "not finished before the readiness deadline" {
			t.Errorf("late %s = %+v, want not finished", c.Name, c)
		}
	}
	if r.Ready() || len(r.Checks) != 7 {
		t.Errorf("report = %+v, want seven checks, not ready", r.Checks)
	}
}

// TestRunKeepsTheWholeRestartBaseline proves the report keeps the restart count of every serving control-plane pod in
// its findings, past the 20-pod preview and however long the pod's name.
func TestRunKeepsTheWholeRestartBaseline(t *testing.T) {
	s, d := ready("127.0.0.1:1")
	long := strings.Repeat("p", 250)
	s.pods["nico"] = []cluster.PodInfo{{Name: long, Ready: true, Restarts: 7}}
	for i := range 21 {
		s.pods["nico"] = append(s.pods["nico"], cluster.PodInfo{Name: fmt.Sprintf("api-%d", i), Ready: true, Restarts: uint32(i)})
	}
	r := readiness.Run(context.Background(), d, time.Now().Add(10*time.Second))
	if len(r.Restarts) != 22 || r.Restarts["nico/"+long] != 7 || !strings.Contains(string(r.Detail()), `"nico/api-20":20`) {
		t.Errorf("baseline = %v, detail %s", r.Restarts, r.Detail())
	}
}

func TestPopulatedNeverReadsNICoDirectly(t *testing.T) {
	s, d := ready("")
	s.records.RegisterExpectedMachines = true
	d.Version = func(context.Context, string) (string, error) {
		t.Error("populated readiness called NICo directly")
		return "", errors.New("forbidden")
	}
	rep := readiness.Populated(context.Background(), d, time.Now().Add(time.Minute))
	if !rep.Ready() {
		t.Fatalf("populated readiness: %+v", rep.Checks)
	}
}
