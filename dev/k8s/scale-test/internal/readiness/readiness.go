// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package readiness runs the fleet-up checks in parallel against one deadline: the probe, the empty site, the metrics
// store, the MAT inventory, the nico-api identity, the control-plane pods and the BMC gate. A check that does not
// answer by the deadline fails.
package readiness

import (
	"cmp"
	"context"
	"encoding/json"
	"fmt"
	"maps"
	"net/netip"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/bmc"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/store"
)

// ExpectedCounts are the units the MAT pods must serve.
type ExpectedCounts struct {
	Racks        uint64 `yaml:"racks"`
	Hosts        uint64 `yaml:"hosts"`
	Switches     uint64 `yaml:"switches"`
	PowerShelves uint64 `yaml:"powerShelves"`
	DPUs         uint64 `yaml:"dpus"`
}

// Settings are what the checks compare against. Release "" fails the identity check.
type Settings struct {
	Namespace    string // where the MAT pods and Services are
	MatSelector  string
	MatPort      uint16
	Expected     ExpectedCounts
	Release      string
	BMC          bmc.Settings
	ControlPlane []config.PodSelector
}

type Prober interface {
	Count(ctx context.Context, f probeapi.Filter) (probeapi.CountResponse, error)
	Config(ctx context.Context, f probe.ConfigFilter) (probeapi.ConfigResponse, error)
}

type StoreClient interface {
	Preflight(ctx context.Context, until time.Time, required ...probeapi.Family) error
	FamilyStatus(ctx context.Context, until time.Time, family probeapi.Family) (store.Status, error)
}

// StoreProbe is a store client and how old a family's newest probe scrape and last sweep end may be: two scrape or
// family reader intervals of the probe, each plus the store's ingestion delay.
type StoreProbe struct {
	Client      StoreClient
	ScrapeBound time.Duration
	SweepBounds map[probeapi.Family]time.Duration
}

type ServiceLister interface {
	ListMatServices(ctx context.Context) ([]cluster.MatService, error)
}

// Deps are the site the checks read. Store builds the store client inside the run, under the deadline, so a store or
// probe metadata failure is a notReady check in the result rather than refusing the run before it records anything;
// nil (checkStore false) is a diagnostic run, which is never go. Identity checks the mounted client certificate;
// Version reaches nico-api at apiURL. Every error they return must be free of reply text.
type Deps struct {
	Probe    Prober
	Store    func(ctx context.Context) (StoreProbe, error)
	Pods     inventory.PodLister
	Records  inventory.RecordsReader
	Services ServiceLister
	Identity func() (string, error)
	Version  func(ctx context.Context, apiURL string) (string, error)
	Dialer   bmc.Dialer
	Settings Settings
	Now      func() time.Time
}

// Check is one named check. Detail says why it failed, or what a pass did not check.
type Check struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail"`
}

// Report is every check, sorted by name, the restart count of every serving control-plane pod ("namespace/pod"),
// the whole baseline an ingestion run must not raise, and the MAT inventory the checks read, which ingestion sends.
type Report struct {
	Checks    []Check
	Restarts  map[string]uint32
	Inventory inventory.SiteInventory
}

// Ready is true when there are checks and every one passed.
func (r Report) Ready() bool {
	return len(r.Checks) > 0 && !slices.ContainsFunc(r.Checks, func(c Check) bool { return !c.OK })
}

// Detail is the report as the fleet-up findings: {"ready": bool, "checks": [...]}.
func (r Report) Detail() json.RawMessage {
	b, _ := json.Marshal(struct {
		Ready    bool              `json:"ready"`
		Checks   []Check           `json:"checks"`
		Restarts map[string]uint32 `json:"controlPlaneRestarts,omitempty"`
	}{r.Ready(), r.Checks, r.Restarts})
	return b
}

// Run reads the MAT inventory, then runs every check in parallel, all bounded by deadline. A check still running at
// the deadline (or a cancel) is failed as not finished and left to stop on its own; an answer taken once the run
// clock reaches the deadline is late, so it is not finished too.
func Run(ctx context.Context, d Deps, deadline time.Time) Report {
	return runChecks(ctx, d, deadline, false)
}

// Populated checks the existing fixture without greenfield or direct NICo reads.
// Cohort selection and dependency checks belong to the instance journey.
func Populated(ctx context.Context, d Deps, deadline time.Time) Report {
	return runChecks(ctx, d, deadline, true)
}

func runChecks(ctx context.Context, d Deps, deadline time.Time, populated bool) Report {
	ctx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	s := d.Settings
	inv := inventory.Read(ctx, d.Pods, s.Namespace, d.Records, s.MatSelector, s.MatPort, populated)
	var restarts map[string]uint32 // read only once its check is taken
	checks := map[string]func() (bool, string){
		"probe":            func() (bool, string) { return d.probeCheck(ctx) },
		"emptySite":        func() (bool, string) { return d.emptySite(ctx) },
		"metricsStore":     func() (bool, string) { return d.storeCheck(ctx, deadline, inv.N) },
		"matInventory":     func() (bool, string) { return matInventory(&inv, s.Expected) },
		"nicoIdentity":     func() (bool, string) { return d.identityCheck(ctx, &inv) },
		"controlPlanePods": func() (ok bool, detail string) { ok, detail, restarts = d.controlPlane(ctx); return },
		"bmc":              func() (bool, string) { return d.bmcCheck(ctx, deadline, inv.BmcCounts) },
	}
	if populated {
		delete(checks, "emptySite")
		delete(checks, "nicoIdentity")
		delete(checks, "bmc")
	}
	done := make(chan Check, len(checks)) // buffered: a check that answers late never blocks
	for name, run := range checks {
		go func() { ok, detail := run(); done <- Check{name, ok, detail} }()
	}
	got := map[string]Check{}
wait:
	for range checks {
		select {
		case c := <-done:
			if ctx.Err() != nil || !d.Now().Before(deadline) {
				break wait
			}
			got[c.Name] = c
		case <-ctx.Done():
			break wait
		}
	}
	r := Report{Inventory: inv}
	for name := range checks {
		late := Check{name, false, "not finished before the readiness deadline"}
		if ctx.Err() == context.Canceled {
			late.Detail = "not finished: cancelled"
		}
		r.Checks = append(r.Checks, cmp.Or(got[name], late))
	}
	slices.SortFunc(r.Checks, func(a, b Check) int { return strings.Compare(a.Name, b.Name) })
	if _, taken := got["controlPlanePods"]; taken {
		r.Restarts = restarts
	}
	return r
}

func (d Deps) probeCheck(ctx context.Context) (bool, string) {
	cfg, err := d.Probe.Config(ctx, probe.ConfigFilter{})
	switch {
	case err != nil:
		return false, "config read: " + err.Error()
	case cfg.Metrics == nil:
		return false, "the probe declares no metrics store"
	case cfg.Metrics.ContractVersion != probeapi.ContractVersion:
		return false, fmt.Sprintf("the probe publishes metric contract %d, the runner reads %d",
			cfg.Metrics.ContractVersion, probeapi.ContractVersion)
	}
	if _, err := d.Probe.Count(ctx, probeapi.Filter{Type: probeapi.UnitTypeHost, Allocatable: new(true)}); err != nil {
		return false, "allocatable host count: " + err.Error()
	}
	return true, ""
}

// emptySite needs a greenfield site: no registered device or native instance and no expected record, each counted
// zero from a fresh sweep of its family. A provider REST total is not read.
func (d Deps) emptySite(ctx context.Context) (bool, string) {
	var fs []probeapi.Filter
	var names []string
	for _, t := range []probeapi.UnitType{probeapi.UnitTypeInstance, probeapi.UnitTypeHost, probeapi.UnitTypeDpu,
		probeapi.UnitTypeSwitch, probeapi.UnitTypePowerShelf, probeapi.UnitTypeRack} {
		fs = append(fs, probeapi.Filter{Type: t, Stage: new(probeapi.StageRegistered)})
		names = append(names, "registered "+string(t))
	}
	for _, r := range []probeapi.RecordType{probeapi.RecordTypeHost, probeapi.RecordTypeSwitch,
		probeapi.RecordTypePowerShelf, probeapi.RecordTypeRack, probeapi.RecordTypeRackGroup} {
		fs = append(fs, probeapi.Filter{Type: probeapi.UnitTypeExpectedRecord, RecordType: new(r)})
		names = append(names, "expected "+string(r))
	}
	problems := make([]string, len(fs))
	var wg sync.WaitGroup
	for i, f := range fs {
		wg.Go(func() {
			switch reply, err := d.Probe.Count(ctx, f); {
			case err != nil:
				problems[i] = err.Error()
			case !probe.Fresh(reply.SweepInfo):
				problems[i] = "no fresh sweep"
			case reply.Count > 0:
				problems[i] = fmt.Sprintf("%d present", reply.Count)
			}
		})
	}
	wg.Wait()
	for i, p := range problems {
		if p != "" {
			return false, names[i] + ": " + p
		}
	}
	return true, ""
}

// storeCheck needs the store to hold the probe process that served the metadata, reading expected records, hosts and
// every other family the inventory plans, each with its newest scrape and last sweep end within its bounds: a scraped
// probe that no longer finishes sweeps, or a store that drops a family, is not ready. A diagnostic run, which does
// not check the store, cannot admit ingestion.
func (d Deps) storeCheck(ctx context.Context, deadline time.Time, n inventory.TypeCounts) (bool, string) {
	if d.Store == nil {
		return false, "metrics store not checked: a diagnostic run (checkStore=false) cannot admit ingestion; set checkStore=true"
	}
	sp, err := d.Store(ctx)
	if err != nil {
		return false, "store client: " + err.Error()
	}
	required := []probeapi.Family{probeapi.FamilyExpected, probeapi.FamilyHost}
	for f, planned := range map[probeapi.Family]uint64{probeapi.FamilyDpu: n.DPU, probeapi.FamilySwitch: n.Switch,
		probeapi.FamilyPowerShelf: n.PowerShelf, probeapi.FamilyRack: n.Rack} {
		if planned > 0 {
			required = append(required, f)
		}
	}
	slices.Sort(required)
	if err := sp.Client.Preflight(ctx, deadline, required...); err != nil {
		return false, "store preflight: " + err.Error()
	}
	within := func(age float64, bound time.Duration) bool { return age >= 0 && age < bound.Seconds() } // false for NaN and ±Inf
	for _, f := range required {
		st, err := sp.Client.FamilyStatus(ctx, deadline, f)
		switch {
		case err != nil:
			return false, string(f) + " status read: " + err.Error()
		case st.SampleAgeSeconds == nil || !within(*st.SampleAgeSeconds, sp.ScrapeBound):
			return false, fmt.Sprintf("no %s probe scrape in the last %v", f, sp.ScrapeBound)
		case st.SweepCompleteAt == nil || !within(d.Now().Sub(st.SweepCompleteAt.Time).Seconds(), sp.SweepBounds[f]):
			return false, fmt.Sprintf("no %s sweep completed in the last %v", f, sp.SweepBounds[f])
		}
	}
	return true, ""
}

func matInventory(inv *inventory.SiteInventory, want ExpectedCounts) (bool, string) {
	if !inv.OK() {
		return false, listed(inv.Problems)
	}
	n := inv.N
	if got := (ExpectedCounts{n.Rack, n.Host, n.Switch, n.PowerShelf, n.DPU}); got != want {
		return false, fmt.Sprintf("served %+v, expected %+v", got, want)
	}
	return true, ""
}

func (d Deps) identityCheck(ctx context.Context, inv *inventory.SiteInventory) (bool, string) {
	if d.Settings.Release == "" {
		return false, "expectedRelease is not set"
	}
	if _, err := d.Identity(); err != nil {
		return false, "client certificate: " + err.Error()
	}
	url, ok := inv.CarbideAPIURL()
	if !ok {
		return false, "the MAT pods name no single nico-api URL"
	}
	version, err := d.Version(ctx, url)
	switch {
	case err != nil:
		return false, "Version call: " + err.Error()
	case version != d.Settings.Release:
		return false, "nico-api serves a release other than expectedRelease" // its build text is not repeated
	}
	return true, ""
}

// controlPlane needs each control-plane selector to match a serving pod, every serving pod Ready, and one of them not
// a MAT pod. A pod that ran to completion or that a Job runs serves nothing and is skipped. It returns the restart
// count of every serving pod, the baseline an ingestion run must not raise; the detail previews it.
func (d Deps) controlPlane(ctx context.Context) (bool, string, map[string]uint32) {
	s := d.Settings
	if len(s.ControlPlane) == 0 {
		return false, "no control-plane selector is configured", nil
	}
	mat, err := d.Pods.ListPods(ctx, s.Namespace, s.MatSelector)
	if err != nil {
		return false, "listing the MAT pods: " + err.Error(), nil
	}
	restarts := map[string]uint32{}
	var problems, baseline []string
	other := false
	for _, sel := range s.ControlPlane {
		pods, err := d.Pods.ListPods(ctx, sel.Namespace, sel.LabelSelector)
		if err != nil {
			problems = append(problems, fmt.Sprintf("listing %s in %s: %s", sel.LabelSelector, sel.Namespace, err))
			continue
		}
		serving := 0
		for _, p := range pods {
			if !Serving(p) {
				continue
			}
			serving++
			if !p.Ready {
				problems = append(problems, fmt.Sprintf("pod %s/%s is not Ready", sel.Namespace, p.Name))
			}
			restarts[sel.Namespace+"/"+p.Name] = p.Restarts
			baseline = append(baseline, fmt.Sprintf("%s/%s: %d restarts", sel.Namespace, p.Name, p.Restarts))
			other = other || sel.Namespace != s.Namespace || !slices.ContainsFunc(mat, func(m cluster.PodInfo) bool { return m.Name == p.Name })
		}
		if serving == 0 {
			problems = append(problems, fmt.Sprintf("no serving pod matches %s in %s", sel.LabelSelector, sel.Namespace))
		}
	}
	switch {
	case len(problems) > 0:
		return false, listed(problems), restarts
	case !other:
		return false, "no control-plane pod besides the MAT pods", restarts
	}
	return true, fmt.Sprintf("%d serving pods Ready, restarts: %s", len(baseline), listed(baseline)), restarts
}

// Serving is false for a pod that ran to completion or that a Job runs: it serves nothing.
func Serving(p cluster.PodInfo) bool {
	_, job := p.Labels["batch.kubernetes.io/job-name"]
	_, old := p.Labels["job-name"]
	return !job && !old && p.Phase != "Succeeded"
}

// bmcCheck reconciles the host and DPU Services with the served inventory, then gates the external IP of each. A
// Service per pod, a Service without exactly one external IP, an IP on two Services, or a kind whose distinct IPs the
// inventory does not serve fails before any BMC is tried, so no device class stands in for another.
func (d Deps) bmcCheck(ctx context.Context, deadline time.Time, want map[string]uint64) (bool, string) {
	services, err := d.Services.ListMatServices(ctx)
	if err != nil {
		return false, "listing MAT Services: " + err.Error()
	}
	var problems []string
	owners, kinds := map[netip.Addr]int{}, map[string]map[netip.Addr]bool{}
	for kind := range want {
		kinds[kind] = map[netip.Addr]bool{}
	}
	for _, s := range services {
		switch {
		case s.MachineType == "pod-bmc" || s.MachineType == "pod-nvos":
			problems = append(problems, "Service "+s.Name+" is per pod: MAT needs serviceGranularity device")
		case s.MachineType != "host" && s.MachineType != "dpu":
		case len(s.ExternalIPs) != 1:
			problems = append(problems, fmt.Sprintf("Service %s has %d external IPs, not 1", s.Name, len(s.ExternalIPs)))
		default:
			kind := inventory.BmcKey(s.MachineType, s.HardwareType)
			if kinds[kind] == nil {
				kinds[kind] = map[netip.Addr]bool{}
			}
			owners[s.ExternalIPs[0]]++
			kinds[kind][s.ExternalIPs[0]] = true
		}
	}
	ips := slices.SortedFunc(maps.Keys(owners), netip.Addr.Compare)
	for _, ip := range ips {
		if owners[ip] > 1 {
			problems = append(problems, fmt.Sprintf("BMC IP %s is on %d Services", ip, owners[ip]))
		}
	}
	for _, kind := range slices.Sorted(maps.Keys(kinds)) {
		if found := uint64(len(kinds[kind])); found != want[kind] {
			problems = append(problems, fmt.Sprintf("found %d distinct %s BMC IPs, the inventory serves %d", found, kind, want[kind]))
		}
	}
	switch {
	case len(problems) > 0:
		return false, listed(problems)
	case len(ips) == 0:
		return false, "at least one expected BMC is needed: no host or DPU Service has an external IP"
	}
	failures := bmc.Check(ctx, ips, d.Dialer, d.Settings.BMC, deadline)
	if len(failures) == 0 {
		return true, ""
	}
	items := make([]string, len(failures))
	for i, f := range failures {
		items[i] = f.IP.String() + ": " + f.Detail
	}
	return false, fmt.Sprintf("%d of %d BMCs failed: %s", len(failures), len(ips), listed(items))
}

// listed joins the first 20 items, each cut to 200 bytes, and counts the rest, so a detail stays small.
func listed(items []string) string {
	shown := make([]string, 0, 21)
	for _, s := range items[:min(len(items), 20)] {
		shown = append(shown, strings.ToValidUTF8(s[:min(len(s), 200)], ""))
	}
	if len(items) > 20 {
		shown = append(shown, fmt.Sprintf("and %d more", len(items)-20))
	}
	return strings.Join(shown, "; ")
}
