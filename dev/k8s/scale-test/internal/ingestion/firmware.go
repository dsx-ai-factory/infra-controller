// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"maps"
	"math"
	"net/url"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"golang.org/x/time/rate"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/timestamppb"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/fleetup"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
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

const firmwareArtifactURL = "https://raw.githubusercontent.com/python/cpython/v3.12.0/LICENSE"
const firmwareArtifactSHA256 = "3b2f81fe21d181c499c59a256c8e1968455d6689d269aa85373bfb6af41da3bf"

// FirmwareSettings uses native readiness and the existing API-pool inputs.
type FirmwareSettings struct {
	fleetup.Settings `yaml:",inline"`
	Observation      struct {
		IntervalSeconds uint64 `yaml:"intervalSeconds"`
	} `yaml:"observation"`
	NativeHealth struct {
		APIPoolSelector map[string]string `yaml:"apiPoolSelector"`
		APIPoolReplicas uint64            `yaml:"apiPoolReplicas"`
	} `yaml:"nativeHealth"`
	Firmware struct {
		N                    uint64             `yaml:"n"`
		Component            probeapi.Component `yaml:"component"`
		TargetVersion        string             `yaml:"targetVersion"`
		ArtifactURL          string             `yaml:"artifactURL"`
		ArtifactSHA256       string             `yaml:"artifactSHA256"`
		WindowSeconds        uint64             `yaml:"windowSeconds"`
		CleanupSeconds       uint64             `yaml:"cleanupSeconds"`
		ManagerScrapeSeconds uint64             `yaml:"managerScrapeSeconds"`
		ReportMaxAgeSeconds  uint64             `yaml:"reportMaxAgeSeconds"`
	} `yaml:"firmware"`
}

func ParseFirmwareSettings(n yaml.Node) (FirmwareSettings, error) {
	s := FirmwareSettings{Settings: fleetup.DefaultSettings()}
	s.Firmware.ArtifactURL = firmwareArtifactURL
	s.Firmware.ArtifactSHA256 = firmwareArtifactSHA256
	s.Firmware.N = 1000
	s.Firmware.Component = probeapi.ComponentBmc
	s.Firmware.WindowSeconds = 7200
	s.Firmware.CleanupSeconds = 3300
	s.Firmware.ReportMaxAgeSeconds = 600
	s.Firmware.ManagerScrapeSeconds = 30
	if err := fleetup.DecodeSettings(n, &s); err != nil {
		return s, err
	}
	s.Firmware.TargetVersion = strings.TrimSpace(s.Firmware.TargetVersion)
	f := s.Firmware
	artifact, e := url.Parse(f.ArtifactURL)
	sha, shaErr := hex.DecodeString(f.ArtifactSHA256)
	if e != nil || artifact.Scheme != "https" || artifact.Hostname() == "" || artifact.User != nil || artifact.RawQuery != "" || artifact.Fragment != "" || shaErr != nil || len(sha) != 32 {
		return s, instanceIssue("firmware requires an HTTPS artifact without credentials and a SHA256 digest")
	}
	if f.N == 0 || f.N > probeapi.MaxCount || (f.Component != probeapi.ComponentBmc && f.Component != probeapi.ComponentUefi) || f.TargetVersion == "" || len(f.TargetVersion) > 4096 || !s.CheckStore || len(s.NativeHealth.APIPoolSelector) == 0 || s.NativeHealth.APIPoolReplicas == 0 {
		return s, instanceIssue("firmware requires N, BMC/UEFI target and API-pool/store evidence")
	}
	for _, v := range []uint64{f.WindowSeconds, f.ReportMaxAgeSeconds} {
		if v == 0 || v > uint64(math.MaxInt64/int64(time.Second)) {
			return s, instanceIssue("firmware duration is outside the supported range")
		}
	}
	if f.ManagerScrapeSeconds < 1 || f.ManagerScrapeSeconds > 3600 {
		return s, instanceIssue("invalid firmware manager scrape interval")
	}
	if f.CleanupSeconds < 1 || f.CleanupSeconds > 3600 || s.Observation.IntervalSeconds < 1 || s.Observation.IntervalSeconds > 86400 {
		return s, instanceIssue("invalid firmware cleanup or observation interval")
	}
	if f.CleanupSeconds < firmwareCleanupMinimum(f.N, s.Observation.IntervalSeconds, 15) {
		return s, instanceIssue("firmware cleanup cannot cover paced closure and two sweeps")
	}
	return s, nil
}

const firmwareAttemptTimeout = time.Minute

// Allow each close its RPC timeout, 2 intervals for ownership proof and 3 for
// closure proof and census while sweep duration stays <= 0.75 intervals.
func firmwareCleanupMinimum(n, interval, poll uint64) uint64 {
	batches := (n + 99) / 100
	return batches*uint64(firmwareAttemptTimeout/time.Second) + (batches+1)/2 + 5*interval + 2*poll + 60
}

type firmwareSnapshot struct {
	Expected map[string][]probeapi.Unit
	Config   probeapi.ConfigResponse
	Rows     map[probeapi.Family]map[string]probeapi.Unit
	Sources  map[probeapi.Family]probeapi.SweepInfo
}

var firmwareFamilies = []struct {
	family probeapi.Family
	kind   probeapi.UnitType
}{
	{probeapi.FamilyHost, probeapi.UnitTypeHost}, {probeapi.FamilyDpu, probeapi.UnitTypeDpu}, {probeapi.FamilyHostFirmwareRequests, probeapi.UnitTypeHostFirmwareRequest}, {probeapi.FamilyExpected, probeapi.UnitTypeExpectedRecord},
}

const errFirmwareSweepDuration instanceIssue = "probe sweep duration exceeds 0.75 x observation interval; raise the probe and observation interval"

const errFirmwareProvenance instanceIssue = "firmware host/config provenance changed"

func firmwareConfig(c probeapi.ConfigResponse, interval uint64) error {
	n := c.NicoAPI
	if c.Metrics == nil || n == nil || n.Revision == nil || *n.Revision == 0 || *n.Revision > probeapi.MaxCount || c.Metrics.ReaderIntervalsSeconds[probeapi.FamilyConfigNicoAPI] != float64(interval) || !firmwareReadFresh(n.ReadInfo, interval) {
		if n != nil && n.ReadAt.Sub(n.ReadStart.Time).Seconds() > .75*float64(interval) {
			return errFirmwareSweepDuration
		}
		return instanceIssue("firmware config revision, cadence or latest attempt unavailable")
	}
	return nil
}
func firmwareReadFresh(r probeapi.ReadInfo, interval uint64) bool {
	return r.LastAttemptError == nil && r.ReadIntervalSeconds == float64(interval) && !r.ReadStart.After(r.ReadAt.Time) && !r.ReadAt.After(time.Now()) && time.Since(r.ReadAt.Time).Seconds() <= 2*float64(interval) && r.ReadAt.Sub(r.ReadStart.Time).Seconds() <= .75*float64(interval)
}

// MR2 revisions change with target/runtime content, and epochs fence restarts.
func firmwareConfigIdentity(c probeapi.ConfigResponse) [2]uint64 {
	return [2]uint64{math.Float64bits(c.Metrics.ProcessStartSeconds), *c.NicoAPI.Revision}
}

func readFirmware(ctx context.Context, p probe.Probe, end time.Time, interval uint64, cleanup bool, previous ...*firmwareSnapshot) (*firmwareSnapshot, error) {
	ctx, cancel := context.WithDeadline(ctx, end)
	defer cancel()
	c, err := p.Config(ctx, probe.ConfigFilter{})
	if err != nil {
		return nil, instanceObservationError(err)
	}
	if c.Metrics == nil {
		return nil, instanceIssue("firmware probe epoch unavailable")
	}
	if !cleanup {
		if err = firmwareConfig(c, interval); err != nil {
			return nil, err
		}
	}
	s := &firmwareSnapshot{Config: c, Rows: map[probeapi.Family]map[string]probeapi.Unit{}, Sources: map[probeapi.Family]probeapi.SweepInfo{}}
	for _, spec := range firmwareFamilies {
		if cleanup && spec.family == probeapi.FamilyHostFirmwareRequests {
			continue
		}
		f := probeapi.Filter{Type: spec.kind, Source: new(probeapi.SourceNicoAPI)}
		if spec.family == probeapi.FamilyExpected {
			f.RecordType = new(probeapi.RecordTypeHost)
		}
		if len(previous) > 0 && previous[0] != nil {
			peek, e := p.Units(ctx, f, nil, probe.Page{Limit: new(uint32(1))})
			if e != nil {
				return nil, instanceObservationError(e)
			}
			e = completedSourceMeta(peek.SweepInfo, spec.family, c.Metrics.ProcessStartSeconds, float64(interval))
			if e == nil && c.Metrics.ProcessStartSeconds == previous[0].Config.Metrics.ProcessStartSeconds && peek.SnapshotID == previous[0].Sources[spec.family].SnapshotID {
				s.Rows[spec.family] = previous[0].Rows[spec.family]
				s.Sources[spec.family] = peek.SweepInfo
				continue
			}
		}
		rows, meta, e := firmwareFamily(ctx, p, end, f, spec.family, c.Metrics.ProcessStartSeconds, float64(interval))
		if e != nil {
			return nil, e
		}
		s.Rows[spec.family] = rows
		s.Sources[spec.family] = meta
	}
	s.Expected = map[string][]probeapi.Unit{}
	for _, u := range s.Rows[probeapi.FamilyExpected] {
		if u.Expected != nil && u.Expected.DeviceID != nil {
			id := *u.Expected.DeviceID
			s.Expected[id] = append(s.Expected[id], u)
		}
	}
	after, err := p.Config(ctx, probe.ConfigFilter{})
	if err != nil {
		return nil, instanceObservationError(err)
	}
	if after.Metrics == nil || after.Metrics.ProcessStartSeconds != c.Metrics.ProcessStartSeconds {
		return nil, instanceObservationError(instanceIssue("firmware probe changed during snapshot"))
	}
	if cleanup {
		return s, nil
	}
	if err = firmwareConfig(after, interval); err != nil {
		return nil, err
	}
	meta := s.Sources[probeapi.FamilyHost]
	pin := meta.FirmwareConfig
	if firmwareConfigIdentity(after) != firmwareConfigIdentity(c) || pin == nil || pin.Revision != *c.NicoAPI.Revision || pin.ReadStart.After(pin.ReadAt.Time) || pin.ReadAt.After(meta.ReadAt.Time) {
		return nil, errFirmwareProvenance
	}
	return s, nil
}
func firmwareFamily(ctx context.Context, p probe.Probe, end time.Time, filter probeapi.Filter, family probeapi.Family, epoch, interval float64) (map[string]probeapi.Unit, probeapi.SweepInfo, error) {
	rows, meta, err := probe.SnapshotUnits(ctx, p, end, filter)
	if err != nil {
		return nil, meta, instanceObservationError(err)
	}
	if err = completedSourceMeta(meta, family, epoch, interval); err != nil {
		current, e := p.Config(ctx, probe.ConfigFilter{})
		if e != nil {
			return nil, meta, instanceObservationError(e)
		}
		if current.Metrics != nil && current.Metrics.ProcessStartSeconds != epoch {
			err = instanceObservationError(err)
		}
		return nil, meta, err
	}
	out := map[string]probeapi.Unit{}
	for _, u := range rows {
		if u.ID == "" || u.Type != filter.Type || u.Source != probeapi.SourceNicoAPI || out[u.ID].ID != "" {
			return nil, meta, instanceIssue("invalid or ambiguous native firmware identity")
		}
		out[u.ID] = u
	}
	return out, meta, nil
}

func closedFirmwareWindow(w *probeapi.FirmwareWindow) bool {
	return w != nil && (w.Start == nil && w.End == nil || w.Start != nil && w.End != nil && w.Start.Unix() == 0 && w.Start.Nanosecond() == 0 && w.End.Unix() == 0 && w.End.Nanosecond() == 0)
}

const errFirmwareCohort instanceIssue = "a firmware cohort host or DPU failed"

func firmwareHostFailed(u probeapi.Unit) bool {
	return u.Stage == probeapi.StageError || u.Host != nil && u.Host.Firmware != nil && slices.ContainsFunc(u.Host.Firmware.Components, func(g probeapi.FirmwareComponentInventory) bool { return g.Phase == probeapi.FirmwarePhaseFailed })
}

func firmwareInventoryUnknown(u probeapi.Unit) bool {
	return u.Host == nil || u.Host.Firmware == nil || len(u.Host.Firmware.Components) == 0 || u.Host.Firmware.WaitingForRefresh != nil && *u.Host.Firmware.WaitingForRefresh || slices.ContainsFunc(u.Host.Firmware.Components, func(g probeapi.FirmwareComponentInventory) bool { return g.Phase == probeapi.FirmwarePhaseUnknown })
}

func firmwareDPUs(s *firmwareSnapshot, u probeapi.Unit) bool {
	if u.Host == nil || len(u.Host.DpuIDs) == 0 {
		return false
	}
	seen := map[string]bool{}
	for _, id := range u.Host.DpuIDs {
		d := s.Rows[probeapi.FamilyDpu][id]
		if seen[id] || d.ID != id || d.Stage != probeapi.StageReady || !d.Done || d.Dpu == nil || d.Dpu.HostID == nil || *d.Dpu.HostID != u.ID {
			return false
		}
		seen[id] = true
	}
	return true
}

// The probe's known phase already proves catalog membership, group/entry
// completeness and exact target matching. Only the benchmark's extra admission
// rule (one coherent behind version per host) belongs here.
func firmwareInventory(u probeapi.Unit, c probeapi.ConfigResponse, component probeapi.Component, target string, baseline bool) bool {
	h := u.Host
	if h == nil || h.Vendor == nil || h.Model == nil || *h.Model != "GB200 NVL" || h.Firmware == nil || h.Firmware.ReportVersion == nil || *h.Firmware.ReportVersion == "" || h.Firmware.WaitingForRefresh == nil || *h.Firmware.WaitingForRefresh || h.ReportError != nil || h.ReportAt == nil || h.VersionSource != probeapi.VersionSourceExploredReport {
		return false
	}
	name := string(component)
	matches := 0
	for _, t := range c.NicoAPI.FirmwareTargets {
		if t.Vendor == *h.Vendor && t.Model == *h.Model && t.Component == component {
			matches++
			if t.ComponentName == nil || t.Version != target || !t.ExplicitStart {
				return false
			}
			name = *t.ComponentName
		}
	}
	if matches != 1 {
		return false
	}
	selected := 0
	for _, g := range h.Firmware.Components {
		if g.ComponentName != name || !baseline {
			if g.Phase != probeapi.FirmwarePhaseCurrent {
				return false
			}
		}
		if g.ComponentName == name {
			selected++
			if baseline {
				if g.Phase != probeapi.FirmwarePhaseBehind || len(g.Entries) == 0 {
					return false
				}
				version := ""
				for _, entry := range g.Entries {
					if entry.Version == nil || *entry.Version == "" || *entry.Version == target || version != "" && *entry.Version != version {
						return false
					}
					version = *entry.Version
				}
			}
		}
	}
	return selected == 1
}

func firmwareEligible(s *firmwareSnapshot, u probeapi.Unit, settings FirmwareSettings) bool {
	return firmwareStructural(s, u, settings) && firmwareInventory(u, s.Config, settings.Firmware.Component, settings.Firmware.TargetVersion, true)
}

// Structural preflight is independent of catalog-matched component inventory.
func firmwareStructural(s *firmwareSnapshot, u probeapi.Unit, settings FirmwareSettings) bool {
	h := u.Host
	return readyHost(u) && firmwareDPUs(s, u) && h.Vendor != nil && *h.Vendor != "" && h.Model != nil && *h.Model == "GB200 NVL" && h.FirmwarePolicyEligible != nil && *h.FirmwarePolicyEligible && h.OpenFirmwareRequest == nil && h.ReportError == nil && h.ReportAt != nil && time.Since(h.ReportAt.Time) >= 0 && time.Since(h.ReportAt.Time).Seconds() <= float64(settings.Firmware.ReportMaxAgeSeconds)
}

// FirmwareTest sets one persistent target and owns one immutable firmware wave.
type FirmwareTest struct {
	cfg   *config.Config
	s     FirmwareSettings
	deps  readiness.Deps
	p     probe.Probe
	check func(context.Context, readiness.Deps, time.Time) readiness.Report
	dial  func(string) (firmwareWriter, error)
	wave  *firmwareRun
}
type firmwareWriter interface {
	UpsertHostFirmwareConfig(context.Context, *corev1.UpsertHostFirmwareConfigRequest) error
	SetFirmwareUpdateTimeWindow(context.Context, *corev1.SetFirmwareUpdateTimeWindowRequest) error
	Close() error
}
type firmwareStore interface {
	readiness.StoreClient
	FirmwareSummary(context.Context, time.Time, probeapi.Filter, probeapi.SummaryParams, uint64) (probeapi.SummaryResponse, error)
	PoolHealthy(context.Context, time.Time, map[string]string) ([]string, error)
	FirmwareManagers(context.Context, time.Time, map[string]string, time.Duration, time.Time) (map[string]store.FirmwareManager, error)
}

func ProductionFirmware(cfg *config.Config, s FirmwareSettings, d readiness.Deps) (*FirmwareTest, error) {
	p, ok := d.Probe.(probe.Probe)
	if !ok {
		return nil, instanceIssue("firmware probe cannot page native evidence")
	}
	return &FirmwareTest{cfg: cfg, s: s, deps: d, p: p, check: readiness.Populated, dial: func(url string) (firmwareWriter, error) { return forge.DialFirmware(url, s.Nico()) }}, nil
}
func (*FirmwareTest) Name() string              { return "firmware" }
func (*FirmwareTest) LockKind() lock.HolderKind { return lock.HolderTest }
func (*FirmwareTest) Writes() bool              { return true }
func (t *FirmwareTest) FirmwareCleanupBudget() time.Duration {
	return time.Duration(t.s.Firmware.CleanupSeconds) * time.Second
}

type firmwarePlan struct {
	Expected                                      map[string][]probeapi.Unit
	Owners                                        map[string]string
	RunID, Site, Component, Vendor, Model, Target string
	N                                             uint64
	Config                                        probeapi.ConfigResponse
	Sources                                       map[probeapi.Family]probeapi.SweepInfo
	Cohort                                        []probeapi.Unit
	Identity                                      map[string]string
	MAT                                           map[string]podState
	Excluded                                      map[string]string
	Base                                          probeapi.SummaryBase
	Start, End                                    int64
}
type firmwareAttempt struct {
	IDs     []string
	Outcome writer.Outcome
	Settled bool
}
type firmwareCompletion struct {
	ID     string
	Latest jsonx.Time
}
type firmwareRun struct {
	cleanupWriter               *writer.Writer
	cleaning                    bool
	opened                      map[string]bool
	pendingSet                  bool
	apiURL                      string
	t                           *FirmwareTest
	latest                      *firmwareSnapshot
	plan                        firmwarePlan
	client                      firmwareWriter
	store                       firmwareStore
	end, cutoff                 time.Time
	cp, mat                     map[string]podState
	pools                       []string
	health                      atomic.Pointer[instanceHealth]
	healthMu                    sync.Mutex
	stopped                     chan struct{}
	stopOnce                    sync.Once
	pace                        *rate.Limiter
	attempts                    []*firmwareAttempt
	t0, lastDispatch, completed time.Time
	done                        map[string]firmwareCompletion
	evidence                    *instanceEvidence
	background                  uint64
	timingInvalid               bool
	detail                      map[string]any
}

func firmwareIdentity(s *firmwareSnapshot, u probeapi.Unit) string {
	if u.Host == nil {
		return ""
	}
	expected := slices.Clone(s.Expected[u.ID])
	slices.SortFunc(expected, func(a, b probeapi.Unit) int { return strings.Compare(a.ID, b.ID) })
	bindings := map[string]string{}
	for _, id := range u.Host.DpuIDs {
		d := s.Rows[probeapi.FamilyDpu][id]
		if d.Dpu != nil && d.Dpu.HostID != nil {
			bindings[id] = *d.Dpu.HostID
		}
	}
	b, _ := json.Marshal([]any{u.ID, u.BmcIP, u.Host.RackID, slices.Sorted(slices.Values(u.Host.DpuIDs)), expected, bindings})
	return digest(b)
}

func (f *firmwareRun) filter() probeapi.Filter {
	return probeapi.Filter{Type: probeapi.UnitTypeHost, Vendor: &f.plan.Vendor, Model: &f.plan.Model, FirmwareComponent: &f.t.s.Firmware.Component, FirmwarePhase: new(probeapi.FirmwarePhaseCurrent), FirmwareReady: new(true)}
}
func (f *firmwareRun) oldest() time.Time {
	oldest := time.Now()
	if !f.cleaning {
		oldest = f.latest.Config.NicoAPI.ReadAt.Time
	}
	for _, s := range f.latest.Sources {
		oldest = minTime(oldest, s.ReadAt.Time)
	}
	return oldest
}
func (f *firmwareRun) latch() { f.stopOnce.Do(func() { close(f.stopped) }) }
func (f *firmwareRun) healthCheck(ctx context.Context, end time.Time) error {
	f.healthMu.Lock()
	defer f.healthMu.Unlock()
	start := time.Now()
	err := func() error {
		ctx, cancel := context.WithDeadline(ctx, end)
		defer cancel()
		c, e := f.t.p.Config(ctx, probe.ConfigFilter{})
		if e != nil {
			return instanceObservationError(e)
		}
		if e = firmwareConfig(c, f.t.s.Observation.IntervalSeconds); e != nil {
			return e
		}
		if firmwareConfigIdentity(c) != firmwareConfigIdentity(f.plan.Config) {
			return instanceIssue("firmware probe process or frozen config changed")
		}
		pools, e := f.store.PoolHealthy(ctx, end, f.t.s.NativeHealth.APIPoolSelector)
		if e != nil {
			return instanceObservationError(e)
		}
		if uint64(len(pools)) != f.t.s.NativeHealth.APIPoolReplicas || f.pools != nil && !slices.Equal(pools, f.pools) {
			return errSafetyStop
		}
		f.pools = pools
		for _, set := range []struct {
			selectors []config.PodSelector
			baseline  map[string]podState
		}{
			{f.t.cfg.ControlPlane, f.cp}, {[]config.PodSelector{{Namespace: f.t.cfg.Namespace, LabelSelector: f.t.s.Mat.Selector}}, f.mat},
		} {
			now := capture(ctx, f.t.deps.Pods, set.selectors, true)
			if now.Fail != "" || !maps.Equal(now.Pods, set.baseline) {
				return errSafetyStop
			}
		}
		return nil
	}()
	f.health.Store(&instanceHealth{at: start, err: err})
	return err
}
func (f *firmwareRun) evidenceGuard(cleanup bool) error {
	if !cleanup {
		h := f.health.Load()
		if h == nil || h.err != nil || time.Since(h.at) < 0 || time.Since(h.at) >= 15*time.Second {
			return errSafetyStop
		}
		select {
		case <-f.stopped:
			return errSafetyStop
		default:
		}
	}
	if time.Since(f.oldest()) < 0 || time.Since(f.oldest()).Seconds() > 2*float64(f.t.s.Observation.IntervalSeconds) {
		return instanceIssue("firmware source evidence expired before dispatch")
	}
	return nil
}
func (f *firmwareRun) refresh(ctx context.Context, end time.Time) error {
	var s *firmwareSnapshot
	var err error
	for {
		s, err = readFirmware(ctx, f.t.p, end, f.t.s.Observation.IntervalSeconds, f.cleaning, f.latest)
		if f.cleaning {
			if gateErr := f.cleanupWriter.Check(ctx); gateErr != nil {
				return gateErr
			}
		}
		var observation instanceObservationFailure
		if !f.cleaning || !errors.As(err, &observation) || !time.Now().Before(end) {
			break
		}
		if err = waitInstance(ctx, f.t.cfg.Probe.PollSeconds); err != nil {
			return err
		}
	}
	if err != nil {
		return err
	}
	if f.cleaning {
		f.latest = s
		return nil
	}
	if firmwareConfigIdentity(s.Config) != firmwareConfigIdentity(f.plan.Config) {
		return instanceIssue("firmware config or process changed")
	}
	changed := []map[string]any{}
	if s.Sources[probeapi.FamilyHost].SnapshotID != f.latest.Sources[probeapi.FamilyHost].SnapshotID {
		old, next := f.latest.Rows[probeapi.FamilyHost], s.Rows[probeapi.FamilyHost]
		record := func(id string, before, after probeapi.Unit, beforePresent, afterPresent bool) error {
			b, _ := json.Marshal(before)
			a, _ := json.Marshal(after)
			if beforePresent == afterPresent && string(b) == string(a) {
				return nil
			}
			_, selected := f.plan.Identity[id]
			if selected && !afterPresent {
				return instanceIssue("firmware cohort host disappeared")
			}
			if !selected {
				f.background++
				if f.currentMetric(before) != f.currentMetric(after) {
					f.timingInvalid = true
				}
			}
			changed = append(changed, map[string]any{"id": id, "before": digest(b), "after": digest(a), "beforePresent": beforePresent, "afterPresent": afterPresent, "selected": selected})
			return nil
		}
		for id, before := range old {
			after, present := next[id]
			if err = record(id, before, after, true, present); err != nil {
				return err
			}
		}
		for id, after := range next {
			if _, present := old[id]; !present {
				if err = record(id, probeapi.Unit{}, after, false, true); err != nil {
					return err
				}
			}
		}
	}
	if len(changed) > 0 && f.evidence != nil {
		if err = f.evidence.append(map[string]any{"sources": s.Sources, "changes": changed}); err != nil {
			return err
		}
	}
	f.latest = s
	return nil
}
func (f *firmwareRun) prepare(ctx context.Context, end time.Time, cleanup bool) error {
	if err := f.pace.Wait(ctx); err != nil {
		return err
	}
	// Reuse immutable families while checking their newest attempt before
	// every write. A changed snapshot downloads once, rather than per batch.
	if err := f.refresh(ctx, end); err != nil {
		return err
	}
	h := f.health.Load()
	if !cleanup && (h == nil || h.err != nil || time.Since(h.at) >= 5*time.Second) {
		if err := f.healthCheck(ctx, end); err != nil {
			f.latch()
			return err
		}
	}
	return f.evidenceGuard(cleanup)
}
func firmwareExactWindow(w *probeapi.FirmwareWindow, start, end int64) bool {
	return w != nil && w.Start != nil && w.End != nil && w.Start.Unix() == start && w.Start.Nanosecond() == 0 && w.End.Unix() == end && w.End.Nanosecond() == 0
}

func (f *firmwareRun) writeGuard(ids []string, cleanup bool) error {
	if err := f.evidenceGuard(cleanup); err != nil {
		return err
	}
	if cleanup {
		for _, id := range ids {
			u := f.latest.Rows[probeapi.FamilyHost][id]
			if !f.opened[id] || firmwareIdentity(f.latest, u) != f.plan.Identity[id] || u.Host == nil || !firmwareExactWindow(u.Host.FirmwareWindow, f.plan.Start, f.plan.End) {
				return instanceIssue("cleanup refuses changed identity or non-owned firmware bounds")
			}
		}
		return nil
	}
	for id := range f.latest.Rows[probeapi.FamilyHostFirmwareRequests] {
		if !f.opened[id] {
			return instanceIssue("firmware request appeared outside the opened cohort")
		}
	}
	for id, u := range f.latest.Rows[probeapi.FamilyHost] {
		h := u.Host
		if h == nil || h.FirmwareWindow == nil {
			return instanceIssue("firmware window is unknown")
		}
		own := f.opened[id]
		if !closedFirmwareWindow(h.FirmwareWindow) && (!own || !firmwareExactWindow(h.FirmwareWindow, f.plan.Start, f.plan.End)) {
			return instanceIssue("foreign firmware window observed")
		}
	}
	for _, baseline := range f.plan.Cohort {
		u := f.latest.Rows[probeapi.FamilyHost][baseline.ID]
		if u.ID == "" || firmwareIdentity(f.latest, u) != f.plan.Identity[u.ID] {
			return instanceIssue("firmware cohort identity changed")
		}
		if !firmwareDPUs(f.latest, u) {
			for _, id := range u.Host.DpuIDs {
				if f.latest.Rows[probeapi.FamilyDpu][id].Stage == probeapi.StageError {
					return errFirmwareCohort
				}
			}
			return instanceIssue("firmware cohort DPU evidence is unhealthy")
		}
		if !f.opened[u.ID] && (!firmwareEligible(f.latest, u, f.t.s) || !closedFirmwareWindow(u.Host.FirmwareWindow)) {
			return instanceIssue("unopened firmware cohort member is no longer eligible")
		}
		if f.opened[u.ID] {
			if firmwareHostFailed(u) {
				return errFirmwareCohort
			}
			if u.Host.Firmware == nil {
				continue
			}
			for _, g := range u.Host.Firmware.Components {
				switch g.Phase {
				case probeapi.FirmwarePhaseBehind, probeapi.FirmwarePhaseUpdating, probeapi.FirmwarePhaseCurrent, probeapi.FirmwarePhaseUnknown:
				default:
					return instanceIssue("opened firmware phase unknown")
				}
			}
		}
	}
	return nil
}
func firmwareRPC(err error) writer.Attempt {
	if err == nil {
		return writer.Attempt{}
	}
	code := status.Code(err)
	if code == codes.Unauthenticated || code == codes.PermissionDenied || code == codes.InvalidArgument {
		r := writer.ClassifyGRPC(code, nil)
		return writer.Attempt{Rejected: &r}
	}
	return writer.Attempt{NoReply: true}
}
func (f *firmwareRun) send(ctx context.Context, w *writer.Writer, ids []string, cleanup bool) (writer.Outcome, error) {
	var none writer.Outcome
	end, cutoff := f.end, f.cutoff
	if cleanup {
		var ok bool
		end, ok = ctx.Deadline()
		if !ok {
			return none, instanceIssue("firmware cleanup requires its own deadline")
		}
		cutoff = end
	}
	if err := f.prepare(ctx, end, cleanup); err != nil {
		return none, err
	}
	if err := f.writeGuard(ids, cleanup); err != nil {
		return none, err
	}
	body := &corev1.SetFirmwareUpdateTimeWindowRequest{StartTimestamp: &timestamppb.Timestamp{Seconds: f.plan.Start}, EndTimestamp: &timestamppb.Timestamp{Seconds: f.plan.End}}
	kind := "firmware-window-open"
	if cleanup {
		kind = "firmware-window-close"
		body.StartTimestamp.Seconds = 0
		body.EndTimestamp.Seconds = 0
	}
	for _, id := range ids {
		body.MachineIds = append(body.MachineIds, &corev1.MachineId{Id: id})
	}
	b, _ := json.Marshal(body)
	call := writer.Call{WriteCall: journal.WriteCall{Scope: "firmware", Kind: kind, Key: kind + ":" + digest(b), Hosts: ids, RequestDigest: digest(b)}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: firmwareAttemptTimeout}, Cutoff: cutoff, BeforeDispatch: func() error { return f.writeGuard(ids, cleanup) }}
	if !cleanup {
		if err := w.Pending(ctx, true); err != nil {
			return none, err
		}
		f.pendingSet = true
	}
	receipt, err := writer.Send(ctx, w, call, func(ctx context.Context) writer.Attempt {
		return firmwareRPC(f.client.SetFirmwareUpdateTimeWindow(ctx, body))
	})
	if err == nil && firmwareUnauthorized(receipt) {
		err = instanceObservationError(errFirmwareUnauthorized)
	}
	var op *writer.OpError
	if errors.As(err, &op) && op.Err == writer.ErrCutoff {
		err = instanceObservationError(err)
	}
	return receipt, err
}
func (f *firmwareRun) awaitBounds(ctx context.Context, w *writer.Writer, a *firmwareAttempt, closing bool, end time.Time) (err error) {
	ctx, cancel := context.WithDeadline(ctx, end)
	defer cancel()
	defer func() {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) && (err == writer.ErrSoftStop || err != nil && instanceExecutionError(err) == nil) {
			err = instanceObservationError(context.DeadlineExceeded)
		}
	}()
	for {
		if err := f.active(ctx, w); err != nil {
			return err
		}
		if !f.latest.Sources[probeapi.FamilyHost].SweepStart.After(a.Outcome.SentAt) {
			if err := f.refresh(ctx, end); err != nil {
				return err
			}
		}
		host := f.latest.Sources[probeapi.FamilyHost]
		fresh := host.SweepStart.After(a.Outcome.SentAt)
		exact := true
		for _, id := range a.IDs {
			u := f.latest.Rows[probeapi.FamilyHost][id]
			if u.Host == nil || firmwareIdentity(f.latest, u) != f.plan.Identity[id] {
				return instanceIssue("firmware receipt identity missing or changed")
			}
			own := firmwareExactWindow(u.Host.FirmwareWindow, f.plan.Start, f.plan.End)
			closed := closedFirmwareWindow(u.Host.FirmwareWindow)
			if !own && !closed {
				return instanceIssue("firmware receipt has foreign or unknown bounds")
			}
			exact = exact && (closing && closed || !closing && own)
		}
		if fresh && exact {
			proof := map[string]any{"ids": a.IDs, "source": host, "closed": closing}
			if a.Outcome.Kind == writer.Unclear {
				if err := w.SettleFirmwareWindow(ctx, a.Outcome.Op, proof); err != nil {
					return err
				}
			} else {
				if err := w.Record(ctx, "firmwareWindowReadBack", proof); err != nil {
					return err
				}
			}
			a.Settled = true
			return nil
		}
		if err := waitInstance(ctx, f.t.cfg.Probe.PollSeconds); err != nil {
			return err
		}
		if err := f.refresh(ctx, end); err != nil {
			return err
		}
	}
}
func firmwareOwnMappings(s *firmwareSnapshot, inv inventory.SiteInventory, mat map[string]podState, namespace string) map[string]string {
	owners := map[string]string{}
	for _, p := range inv.Pods {
		if p.UID == "" || mat[namespace+"/"+p.Name].UID != p.UID || !p.Ready {
			continue
		}
		for _, d := range p.Records.Devices {
			if d.Kind == inventory.KindMachine && d.RackID != nil {
				key := strings.ToLower(d.BmcMac)
				if owners[key] != "" {
					owners[key] = "ambiguous"
				} else {
					owners[key] = p.Name + ":" + p.UID + ":" + *d.RackID
				}
			}
		}
	}
	linked := map[string]string{}
	for _, e := range s.Rows[probeapi.FamilyExpected] {
		x := e.Expected
		if x == nil || !x.Linked || x.DeviceID == nil || x.BmcMac == nil {
			continue
		}
		u := s.Rows[probeapi.FamilyHost][*x.DeviceID]
		owner := owners[strings.ToLower(*x.BmcMac)]
		if owner == "" || owner == "ambiguous" || u.BmcIP == nil || *u.BmcIP == "" || u.Host == nil || u.Host.RackID == nil || !strings.HasSuffix(owner, ":"+*u.Host.RackID) || linked[u.ID] != "" {
			linked[*x.DeviceID] = "ambiguous"
			continue
		}
		linked[u.ID] = owner
	}
	return linked
}

func (t *FirmwareTest) Run(rc run.RunCtx, w *writer.Writer) (out result.Outcome, err error) {
	out = result.OutcomeOf(result.NotReady)
	end := rc.Clock.Instant().Add(rc.Deadline.Sub(rc.Clock.Now()))
	ctx, cancel := context.WithDeadline(rc.Ctx, end)
	defer cancel()
	f := &firmwareRun{t: t, end: end, stopped: make(chan struct{}), pace: rate.NewLimiter(2, 1), done: map[string]firmwareCompletion{}, detail: map[string]any{}, opened: map[string]bool{}}
	t.wave = f
	defer func() {
		var pe *probe.Error
		var issue instanceIssue
		cancelled := errors.Is(err, context.Canceled) || errors.As(err, &pe) && pe.Kind == probe.Cancelled
		if errors.Is(ctx.Err(), context.DeadlineExceeded) && !errors.As(err, &issue) && (err == writer.ErrSoftStop || cancelled && instanceExecutionError(err) == nil) {
			err = instanceObservationError(context.DeadlineExceeded)
		}
		err = errors.Join(err, rc.Ctx.Err())
		if errors.Is(err, errFirmwareCohort) {
			out.Verdict = result.Failed
		}
		if err != nil {
			out.Reasons = append(out.Reasons, why(err))
		}
		err = instanceExecutionError(err)
		failed := uint64(0)
		if f.latest != nil {
			for _, base := range f.plan.Cohort {
				u := f.latest.Rows[probeapi.FamilyHost][base.ID]
				isFailed := firmwareHostFailed(u)
				if u.Host != nil {
					for _, id := range u.Host.DpuIDs {
						isFailed = isFailed || f.latest.Rows[probeapi.FamilyDpu][id].Stage == probeapi.StageError
					}
				}
				if isFailed {
					failed++
					delete(f.done, base.ID)
				}
			}
		}
		stage := result.StageUnresolved
		switch out.Verdict {
		case result.Passed:
			stage = result.StageDone
		case result.Failed:
			stage = result.StageFailed
		}
		out.Stages = []result.StageResult{{ID: result.StageID("firmware"), Status: stage, Units: result.UnitCountsOf(t.s.Firmware.N, uint64(len(f.done)), failed)}}
		if rc.DryRun {
			out.Stages = nil
		}
		f.detail["catalogRestorePolicy"] = "never"
		f.detail["catalogTarget"] = t.s.Firmware.TargetVersion
		f.detail["done"] = len(f.done)
		f.detail["n"] = t.s.Firmware.N
		f.detail["backgroundChanges"] = f.background
		f.detail["t0"] = f.t0
		f.detail["lastDispatch"] = f.lastDispatch
		f.detail["completedAt"] = f.completed
		if f.evidence != nil {
			e := f.evidence.finish()
			err = errors.Join(err, e)
			f.detail["transitions"] = map[string]any{"path": f.evidence.path, "sha256": f.evidence.hash, "entries": f.evidence.count}
			f.evidence = nil
		}
		out.Detail, _ = json.Marshal(f.detail)
	}()
	if t.cfg.Probe.PollSeconds >= 2*t.s.Observation.IntervalSeconds {
		return out, instanceIssue("probe poll must be shorter than the firmware ownership grace")
	}
	if t.cfg.Probe.PollSeconds > 3600 || t.s.Firmware.CleanupSeconds < firmwareCleanupMinimum(t.s.Firmware.N, t.s.Observation.IntervalSeconds, t.cfg.Probe.PollSeconds) {
		return out, instanceIssue("firmware cleanup cannot cover paced closure and fresh proof sweeps")
	}
	d := t.deps
	d.Now = rc.Clock.Now
	readinessEnd := minTime(end, rc.Clock.Instant().Add(time.Duration(t.cfg.Timeouts.ReadinessSeconds)*time.Second))
	rep := t.check(ctx, d, readinessEnd)
	if !rep.Ready() {
		f.detail["readiness"] = json.RawMessage(rep.Detail())
		return out, instanceIssue("firmware native readiness refused")
	}
	sp, e := t.deps.Store(ctx)
	if e != nil {
		return out, instanceObservationError(e)
	}
	var ok bool
	f.store, ok = sp.Client.(firmwareStore)
	if !ok {
		return out, instanceIssue("store lacks firmware evidence support")
	}
	f.cp = capture(ctx, t.deps.Pods, t.cfg.ControlPlane, true).Pods
	f.mat = capture(ctx, t.deps.Pods, []config.PodSelector{{Namespace: t.cfg.Namespace, LabelSelector: t.s.Mat.Selector}}, true).Pods
	first, e := readFirmware(ctx, t.p, readinessEnd, t.s.Observation.IntervalSeconds, false)
	if e != nil {
		return out, e
	}
	f.plan.Config = first.Config
	f.latest = first
	f.evidence, e = openEvidence(t.cfg.Journal.Dir, t.cfg.RunID, 128<<20)
	if e != nil {
		return out, e
	}
	for _, u := range first.Rows[probeapi.FamilyHost] {
		if u.Host == nil || !closedFirmwareWindow(u.Host.FirmwareWindow) {
			return out, instanceIssue("firmware admission found a foreign or unknown window")
		}
	}
	if len(first.Rows[probeapi.FamilyHostFirmwareRequests]) != 0 {
		return out, instanceIssue("firmware requires no outstanding requests")
	}
	runtime := first.Config.NicoAPI.Runtime
	if runtime.AutoHostFirmwareUpdate == nil || !*runtime.AutoHostFirmwareUpdate || runtime.RequiresManualUpgrade == nil || *runtime.RequiresManualUpgrade || runtime.MachineUpdateRunIntervalSeconds == nil || *runtime.MachineUpdateRunIntervalSeconds == 0 || *runtime.MachineUpdateRunIntervalSeconds > 86400 || runtime.MaxConcurrentMachineUpdatesAbsolute == nil && runtime.MaxConcurrentMachineUpdatesPercent == nil || runtime.MaxConcurrentMachineUpdatesAbsolute != nil && *runtime.MaxConcurrentMachineUpdatesAbsolute <= 0 || runtime.MaxConcurrentMachineUpdatesPercent != nil && *runtime.MaxConcurrentMachineUpdatesPercent <= 0 {
		return out, instanceIssue("firmware runtime policy, cap or manager interval unknown or disabled")
	}
	managers, e := f.store.FirmwareManagers(ctx, readinessEnd, t.s.NativeHealth.APIPoolSelector, time.Duration(t.s.Firmware.ManagerScrapeSeconds)*time.Second, time.Time{})
	if e != nil {
		return out, instanceObservationError(e)
	}
	if !slices.ContainsFunc(slices.Collect(maps.Keys(managers)), func(id string) bool { return managers[id].Cap > 0 && f.cp[id].UID != "" }) {
		return out, instanceIssue("no qualified manager reports a positive effective cap")
	}
	url, ok := rep.Inventory.CarbideAPIURL()
	if !ok {
		return out, instanceIssue("firmware native API identity is ambiguous")
	}
	f.apiURL = url
	f.client, e = t.dial(url)
	if e != nil {
		return out, e
	}
	defer func() { _ = f.client.Close() }()
	if e = f.catalog(ctx, w, readinessEnd, rep.Inventory, rc.DryRun); e != nil {
		return out, e
	}
	if rc.DryRun {
		return result.OutcomeOf(result.Passed), nil
	}
	readinessEnd = minTime(end, time.Now().Add(time.Duration(t.cfg.Timeouts.ReadinessSeconds)*time.Second))
	first = f.latest
	runtime = first.Config.NicoAPI.Runtime
	unstable := map[string]bool{}
	for {
		if e = f.refresh(ctx, readinessEnd); e != nil {
			return out, e
		}
		if len(f.latest.Rows[probeapi.FamilyHostFirmwareRequests]) != 0 {
			return out, instanceIssue("firmware requires no outstanding requests")
		}
		advanced := true
		for _, spec := range firmwareFamilies {
			advanced = advanced && f.latest.Sources[spec.family].SweepStart.After(first.Sources[spec.family].ReadAt.Time)
		}
		for id, u := range f.latest.Rows[probeapi.FamilyHost] {
			if u.Host == nil || !closedFirmwareWindow(u.Host.FirmwareWindow) {
				return out, instanceIssue("firmware admission found a foreign or unknown window")
			}
			if !firmwareEligible(f.latest, u, t.s) || firmwareIdentity(f.latest, u) != firmwareIdentity(first, first.Rows[probeapi.FamilyHost][id]) {
				unstable[id] = true
			}
		}
		if advanced {
			break
		}
		if e = waitInstance(ctx, t.cfg.Probe.PollSeconds); e != nil {
			return out, e
		}
	}
	f.plan = firmwarePlan{RunID: t.cfg.RunID, Site: t.cfg.Site.LockName, Component: string(t.s.Firmware.Component), Target: t.s.Firmware.TargetVersion, N: t.s.Firmware.N, Config: first.Config, Sources: f.latest.Sources, Identity: map[string]string{}, MAT: f.mat, Excluded: map[string]string{}, Expected: map[string][]probeapi.Unit{}}
	owners := firmwareOwnMappings(f.latest, rep.Inventory, f.mat, t.cfg.Namespace)
	f.plan.Owners = map[string]string{}
	for _, id := range slices.Sorted(maps.Keys(f.latest.Rows[probeapi.FamilyHost])) {
		u := f.latest.Rows[probeapi.FamilyHost][id]
		if unstable[id] || !firmwareEligible(first, first.Rows[probeapi.FamilyHost][id], t.s) || !firmwareEligible(f.latest, u, t.s) || owners[id] == "" || owners[id] == "ambiguous" {
			f.plan.Excluded[id] = "not stable, healthy, linked or target-behind"
			continue
		}
		vendor := *u.Host.Vendor
		if f.plan.Vendor != "" && f.plan.Vendor != vendor {
			return out, instanceIssue("firmware eligible census contains multiple vendors")
		}
		f.plan.Vendor = vendor
		f.plan.Model = *u.Host.Model
		if uint64(len(f.plan.Cohort)) < f.plan.N {
			f.plan.Cohort = append(f.plan.Cohort, u)
			f.plan.Expected[id] = f.latest.Expected[id]
			f.plan.Identity[id] = firmwareIdentity(f.latest, u)
			f.plan.Owners[id] = owners[id]
		}
	}
	if uint64(len(f.plan.Cohort)) != f.plan.N {
		return out, instanceIssue("firmware N exceeds the exact eligible census")
	}
	interval := t.s.Observation.IntervalSeconds
	if t.s.Firmware.WindowSeconds <= t.s.Firmware.CleanupSeconds+2*interval+2**runtime.MachineUpdateRunIntervalSeconds+60 {
		return out, instanceIssue("firmware window leaves no opening/verification reserve")
	}
	start := time.Now().UTC().Unix()
	if start < 0 || t.s.Firmware.WindowSeconds > uint64(253402300799-start) {
		return out, instanceIssue("firmware window timestamp overflow")
	}
	f.plan.Start = start
	f.plan.End = start + int64(t.s.Firmware.WindowSeconds)
	reserve := time.Duration(2*interval+2**runtime.MachineUpdateRunIntervalSeconds)*time.Second + t.FirmwareCleanupBudget()
	f.cutoff = minTime(end.Add(-reserve), time.Now().Add(time.Until(time.Unix(f.plan.End, 0).Add(-reserve))))
	if !time.Now().Before(f.cutoff) {
		return out, instanceIssue("firmware run leaves no opening reserve")
	}
	if e = f.healthCheck(ctx, readinessEnd); e != nil {
		return out, e
	}
	count, e := t.p.Count(ctx, f.filter())
	if e != nil {
		return out, instanceObservationError(e)
	}
	if count.SnapshotID != f.latest.Sources[probeapi.FamilyHost].SnapshotID {
		return out, instanceIssue("firmware baseline count and host snapshot differ")
	}
	exact := uint64(0)
	for _, u := range f.latest.Rows[probeapi.FamilyHost] {
		if f.currentMetric(u) {
			exact++
		}
	}
	if count.Count != exact {
		return out, instanceIssue("firmware baseline count differs from exact native evidence")
	}
	f.plan.Base = probeapi.SummaryBase{Count: count.Count, ProcessStartSeconds: first.Config.Metrics.ProcessStartSeconds, SweepStart: count.SweepStart, ReadAt: count.ReadAt}
	baseStart := count.ReadAt.Add(time.Nanosecond)
	base, e := f.store.FirmwareSummary(ctx, readinessEnd, f.filter(), probeapi.SummaryParams{Start: jsonx.Time{Time: baseStart}, End: jsonx.Time{Time: time.Now()}, N: 0, Base: &f.plan.Base}, *first.Config.NicoAPI.Revision)
	if e != nil {
		return out, instanceObservationError(e)
	}
	if base.FinalCount == nil || *base.FinalCount != count.Count {
		return out, instanceIssue("stored firmware baseline is missing or disagrees")
	}
	name := t.cfg.RunID + ".firmware-plan.json"
	hash, e := writeOnce(t.cfg.Journal.Dir, name, f.plan)
	if e != nil {
		return out, e
	}
	f.detail["plan"] = map[string]string{"path": name, "sha256": hash}
	if e = w.Record(ctx, "firmwarePlan", f.detail["plan"]); e != nil {
		return out, e
	}
	out.Verdict = result.Unresolved
	healthCtx, stopHealth := context.WithCancel(ctx)
	healthDone := make(chan struct{})
	go func() {
		defer close(healthDone)
		tick := time.NewTicker(5 * time.Second)
		defer tick.Stop()
		for {
			select {
			case <-healthCtx.Done():
				return
			case <-tick.C:
				if f.healthCheck(healthCtx, end) != nil {
					f.latch()
					return
				}
			}
		}
	}()
	defer func() { stopHealth(); <-healthDone }()
	for i := 0; i < len(f.plan.Cohort); i += 100 {
		ids := []string{}
		for _, u := range f.plan.Cohort[i:min(i+100, len(f.plan.Cohort))] {
			ids = append(ids, u.ID)
		}
		receipt, e := f.send(ctx, w, ids, false)
		if !receipt.SentAt.IsZero() {
			a := &firmwareAttempt{IDs: ids, Outcome: receipt}
			f.attempts = append(f.attempts, a)
			if receipt.Kind != writer.Failed {
				for _, id := range ids {
					f.opened[id] = true
				}
			}
			if f.t0.IsZero() {
				f.t0 = receipt.SentAt
			}
			f.lastDispatch = receipt.SentAt
			if e == nil && receipt.Kind == writer.Unclear {
				e = f.awaitBounds(ctx, w, a, false, minTime(end, time.Now().Add(2*time.Duration(t.s.Observation.IntervalSeconds)*time.Second)))
			}
		}
		if e != nil {
			return out, e
		}
		if receipt.Kind != writer.Accepted && receipt.Kind != writer.Unclear {
			return out, instanceIssue("firmware window setter refused")
		}
	}
	if e = f.settleAttempts(ctx, w, f.attempts, false, end); e != nil {
		return out, e
	}
	if e = f.observeCompletion(ctx, end, w); e != nil {
		return out, e
	}
	if e = f.verifyPasses(ctx, end, w); e != nil {
		return out, e
	}
	summary, e := f.store.FirmwareSummary(ctx, rc.ProbeReadEnd, f.filter(), probeapi.SummaryParams{Start: jsonx.Time{Time: f.t0}, End: jsonx.Time{Time: f.completed}, N: f.plan.N, Base: &f.plan.Base, AddWindowSeconds: f.lastDispatch.Sub(f.t0).Seconds()}, *f.plan.Config.NicoAPI.Revision)
	if e != nil {
		return out, instanceObservationError(e)
	}
	f.detail["timing"] = summary
	if f.timingInvalid || !summary.FinalScrapeSeen || summary.TimingErrorSeconds == nil || summary.FinalCount == nil || *summary.FinalCount != f.plan.Base.Count+f.plan.N || len(summary.Crossings) != len(probeapi.FixedQuantiles) || slices.ContainsFunc(summary.Crossings, func(c probeapi.Crossing) bool { return c.Seconds == nil }) {
		return out, instanceIssue("firmware timing unavailable or changed outside its cohort")
	}
	out.Verdict = result.Passed
	return out, nil
}

func (f *firmwareRun) currentMetric(u probeapi.Unit) bool {
	if !readyHost(u) || u.Host.Firmware == nil || u.Host.Vendor == nil || u.Host.Model == nil || *u.Host.Vendor != f.plan.Vendor || *u.Host.Model != f.plan.Model {
		return false
	}
	for _, t := range f.plan.Config.NicoAPI.FirmwareTargets {
		if t.Vendor == f.plan.Vendor && t.Model == f.plan.Model && t.Component == f.t.s.Firmware.Component && t.ComponentName != nil {
			for _, g := range u.Host.Firmware.Components {
				if g.ComponentName == *t.ComponentName {
					return g.Phase == probeapi.FirmwarePhaseCurrent
				}
			}
		}
	}
	return false
}
func (f *firmwareRun) observeCompletion(ctx context.Context, end time.Time, w *writer.Writer) error {
	for {
		if err := f.active(ctx, w); err != nil {
			return err
		}
		if err := f.refresh(ctx, end); err != nil {
			return err
		}
		if err := f.writeGuard(nil, false); err != nil {
			return err
		}
		outside := uint64(0)
		for id, u := range f.latest.Rows[probeapi.FamilyHost] {
			if _, owned := f.plan.Identity[id]; !owned && f.currentMetric(u) {
				outside++
			}
		}
		if outside != f.plan.Base.Count {
			f.timingInvalid = true
		}
		for _, base := range f.plan.Cohort {
			u := f.latest.Rows[probeapi.FamilyHost][base.ID]
			h := u.Host
			if _, done := f.done[u.ID]; done {
				if firmwareInventoryUnknown(u) {
					delete(f.done, u.ID)
					continue
				}
				if !readyHost(u) || !firmwareInventory(u, f.plan.Config, f.t.s.Firmware.Component, f.plan.Target, false) {
					return instanceIssue("completed firmware cohort evidence changed")
				}
				continue
			}
			if readyHost(u) && firmwareDPUs(f.latest, u) && firmwareInventory(u, f.plan.Config, f.t.s.Firmware.Component, f.plan.Target, false) && h.ReportAt.After(f.t0) && time.Since(h.ReportAt.Time) >= 0 && time.Since(h.ReportAt.Time).Seconds() <= float64(f.t.s.Firmware.ReportMaxAgeSeconds) && *h.Firmware.ReportVersion != *base.Host.Firmware.ReportVersion {
				source := f.latest.Sources[probeapi.FamilyHost]
				f.done[u.ID] = firmwareCompletion{ID: u.ID, Latest: source.ReadAt}
			}
		}
		if len(f.done) == len(f.plan.Cohort) {
			if f.evidence != nil {
				if err := f.evidence.append(map[string]any{"completions": f.done}); err != nil {
					return err
				}
			}

			f.completed = f.latest.Sources[probeapi.FamilyHost].ReadAt.Time
			return nil
		}
		if err := waitInstance(ctx, f.t.cfg.Probe.PollSeconds, f.stopped); err != nil {
			return err
		}
	}
}
func (f *firmwareRun) verifyPasses(ctx context.Context, end time.Time, w *writer.Writer) error {
	baselines, previous := map[string]store.FirmwareManager{}, map[string]store.FirmwareManager{}
	pod := ""
	var provenAt time.Time
	for {
		if err := f.active(ctx, w); err != nil {
			return err
		}
		if err := f.healthCheck(ctx, end); err != nil {
			return err
		}
		since := time.Time{}
		for _, v := range previous {
			if since.IsZero() || v.At.Before(since) {
				since = v.At
			}
		}
		managers, err := f.store.FirmwareManagers(ctx, end, f.t.s.NativeHealth.APIPoolSelector, time.Duration(f.t.s.Firmware.ManagerScrapeSeconds)*time.Second, since)
		if err != nil {
			return instanceObservationError(err)
		}
		for id, last := range previous {
			v, ok := managers[id]
			if !ok || v.Cap == 0 || v.Passes < last.Passes || v.At.Before(last.At) || v.From.After(last.At) {
				return instanceIssue("firmware manager identity, gap or counter reset")
			}
		}
		for _, id := range slices.Sorted(maps.Keys(managers)) {
			v := managers[id]
			if f.cp[id].UID == "" || v.Cap == 0 || v.At.Before(f.completed) {
				continue
			}
			baseline, ok := baselines[id]
			if !ok {
				baselines[id] = v
				previous[id] = v
				continue
			}
			previous[id] = v
			if pod == "" && v.Passes >= baseline.Passes+2 && v.At.After(baseline.At) {
				pod, provenAt = id, v.At
			}
		}
		if pod != "" {
			if err = f.refresh(ctx, end); err != nil {
				return err
			}
			if err = f.writeGuard(nil, false); err != nil {
				return err
			}
			if f.latest.Sources[probeapi.FamilyHost].SweepStart.After(provenAt) && f.latest.Sources[probeapi.FamilyHostFirmwareRequests].SweepStart.After(provenAt) {
				if len(f.latest.Rows[probeapi.FamilyHostFirmwareRequests]) != 0 {
					return instanceIssue("firmware verification still has active requests")
				}
				complete := true
				for _, base := range f.plan.Cohort {
					u := f.latest.Rows[probeapi.FamilyHost][base.ID]
					if firmwareInventoryUnknown(u) {
						complete = false
						break
					}
					if !readyHost(u) || !firmwareDPUs(f.latest, u) || firmwareIdentity(f.latest, u) != f.plan.Identity[u.ID] || !firmwareInventory(u, f.plan.Config, f.t.s.Firmware.Component, f.plan.Target, false) || !u.Host.UpdateComplete || u.Host.UpdateEligible == nil || *u.Host.UpdateEligible || u.Host.OpenFirmwareRequest != nil {
						return instanceIssue("firmware post-pass completion is missing or still eligible")
					}
				}
				if complete {
					v, baseline := managers[pod], baselines[pod]
					f.detail["verification"] = map[string]any{"manager": pod, "uid": f.cp[pod].UID, "baseline": baseline, "after": v, "passes": v.Passes - baseline.Passes}
					return nil
				}
			}
		}
		if err = waitInstance(ctx, f.t.cfg.Probe.PollSeconds, f.stopped); err != nil {
			return err
		}
	}
}

// CloseFirmwareWindows is the sole cleanup path, including normal completion.
// It never changes catalog/policy, replays an opening, or adopts foreign bounds.
func (t *FirmwareTest) CloseFirmwareWindows(ctx context.Context, w *writer.Writer) (detail json.RawMessage, err error) {
	defer func() {
		if err != nil {
			cause := ctx.Err()
			// A same-deadline child read can finish before the parent timer fires.
			if until, ok := ctx.Deadline(); cause == nil && ok && !time.Now().Before(until) {
				cause = context.DeadlineExceeded
			}
			err = errors.Join(err, cause)
		}
	}()
	f := t.wave
	if f == nil || !f.pendingSet {
		return json.RawMessage(`{"status":"no-attempts","pending":false}`), nil
	}
	end, ok := ctx.Deadline()
	if !ok {
		return nil, instanceIssue("firmware cleanup has no deadline")
	}
	f.cleanupWriter = w
	f.cleaning = true
	f.pace = rate.NewLimiter(2, 1)
	defer func() { f.cleaning = false }()
	if err := f.refresh(ctx, end); err != nil {
		return nil, err
	}
	client, err := t.dial(f.apiURL)
	if err != nil {
		return nil, err
	}
	defer func() { _ = client.Close() }()
	f.client = client
	graceAfter := f.latest.Sources[probeapi.FamilyHost].ReadAt.Time
	for _, a := range f.attempts {
		if a.Outcome.SentAt.After(graceAfter) {
			graceAfter = a.Outcome.SentAt
		}
	}
	graceEnd := minTime(end, time.Now().Add(2*time.Duration(t.s.Observation.IntervalSeconds)*time.Second))
	observed, sent := map[string]bool{}, map[string]bool{}
	closes := []*firmwareAttempt{}
	var blocked error
	// Close proven windows before granting absent unclear opens one shared sweep.
rounds:
	for round := 0; round < 2; round++ {
		open := []string{}
		for _, a := range f.attempts {
			if a.Outcome.Kind == writer.Failed {
				continue
			}
			fresh := f.latest.Sources[probeapi.FamilyHost].SweepStart.After(a.Outcome.SentAt)
			for _, id := range a.IDs {
				u := f.latest.Rows[probeapi.FamilyHost][id]
				if !fresh {
					continue
				}
				if u.Host == nil || firmwareIdentity(f.latest, u) != f.plan.Identity[id] || u.Host.FirmwareWindow == nil {
					blocked = instanceIssue("cleanup identity or window unknown")
					continue
				}
				if firmwareExactWindow(u.Host.FirmwareWindow, f.plan.Start, f.plan.End) {
					observed[id] = true
					if !sent[id] {
						open = append(open, id)
					}
				} else if !closedFirmwareWindow(u.Host.FirmwareWindow) {
					blocked = instanceIssue("cleanup refuses foreign firmware bounds")
				}
			}
			all := true
			for _, id := range a.IDs {
				all = all && observed[id]
			}
			if all && !a.Settled {
				proof := map[string]any{"ids": a.IDs, "source": f.latest.Sources[probeapi.FamilyHost], "ownedReadBacks": "firmwareOwnedWindowProof"}
				if a.Outcome.Kind == writer.Unclear {
					err = w.SettleFirmwareWindow(ctx, a.Outcome.Op, proof)
				} else {
					err = w.Record(ctx, "firmwareWindowReadBack", proof)
				}
				if err != nil {
					return nil, err
				}
				a.Settled = true
			}
		}
		if len(open) > 0 {
			if err = w.Record(ctx, "firmwareOwnedWindowProof", map[string]any{"ids": open, "source": f.latest.Sources[probeapi.FamilyHost]}); err != nil {
				return nil, err
			}
		}
		for i := 0; i < len(open); i += 100 {
			batch := open[i:min(i+100, len(open))]
			receipt, e := f.send(ctx, w, batch, true)
			if !receipt.SentAt.IsZero() {
				for _, id := range batch {
					sent[id] = true
				}
				closes = append(closes, &firmwareAttempt{IDs: batch, Outcome: receipt})
			}
			if e != nil {
				return nil, e
			}
			if receipt.Kind == writer.Failed {
				return nil, instanceIssue("firmware window closure refused")
			}
		}
		unresolved := false
		for _, a := range f.attempts {
			unresolved = unresolved || a.Outcome.Kind != writer.Failed && !a.Settled
		}
		if !unresolved || round == 1 {
			break
		}
		for !f.latest.Sources[probeapi.FamilyHost].SweepStart.After(graceAfter) {
			if !time.Now().Before(graceEnd) {
				blocked = errors.Join(blocked, context.DeadlineExceeded)
				break rounds
			}
			if err = waitInstance(ctx, t.cfg.Probe.PollSeconds); err != nil {
				blocked = errors.Join(blocked, err)
				break rounds
			}
			if err = f.refresh(ctx, graceEnd); err != nil {
				blocked = errors.Join(blocked, err)
				break rounds
			}
		}
	}
	if err = f.settleAttempts(ctx, w, closes, true, end); err != nil {
		return nil, errors.Join(blocked, err)
	}
	if blocked != nil {
		return nil, blocked
	}
	for _, a := range f.attempts {
		if a.Outcome.Kind != writer.Failed && !a.Settled {
			return nil, instanceIssue("firmware opening remains unresolved")
		}
	}
	// Clean release needs a later host/request census, independently of closure.
	proofAt := f.latest.Sources[probeapi.FamilyHost].ReadAt.Time
	for {
		if err = f.refresh(ctx, end); err != nil {
			return nil, err
		}
		requests, source, e := firmwareFamily(ctx, t.p, end, probeapi.Filter{Type: probeapi.UnitTypeHostFirmwareRequest, Source: new(probeapi.SourceNicoAPI)}, probeapi.FamilyHostFirmwareRequests, f.latest.Config.Metrics.ProcessStartSeconds, float64(t.s.Observation.IntervalSeconds))
		if e != nil {
			if err = w.Check(ctx); err != nil {
				return nil, err
			}
			var observation instanceObservationFailure
			if errors.As(e, &observation) && time.Now().Before(end) {
				if err = waitInstance(ctx, t.cfg.Probe.PollSeconds); err != nil {
					return nil, err
				}
				continue
			}
			return nil, e
		}
		f.latest.Rows[probeapi.FamilyHostFirmwareRequests] = requests
		f.latest.Sources[probeapi.FamilyHostFirmwareRequests] = source
		if f.latest.Sources[probeapi.FamilyHost].SweepStart.After(proofAt) && source.SweepStart.After(proofAt) {
			break
		}
		if err = waitInstance(ctx, t.cfg.Probe.PollSeconds); err != nil {
			return nil, err
		}
	}
	for _, a := range f.attempts {
		if a.Outcome.Kind == writer.Failed {
			continue
		}
		for _, id := range a.IDs {
			u := f.latest.Rows[probeapi.FamilyHost][id]
			if u.Host == nil || firmwareIdentity(f.latest, u) != f.plan.Identity[id] || !closedFirmwareWindow(u.Host.FirmwareWindow) || !readyHost(u) || !firmwareDPUs(f.latest, u) || u.Host.OpenFirmwareRequest != nil || u.Host.ReportError != nil || u.Host.Firmware == nil || u.Host.Firmware.WaitingForRefresh == nil || *u.Host.Firmware.WaitingForRefresh {
				return nil, instanceIssue("firmware closure still has unknown active work")
			}
			if _, active := f.latest.Rows[probeapi.FamilyHostFirmwareRequests][id]; active {
				return nil, instanceIssue("firmware closure did not cancel outstanding work")
			}
		}
	}
	if err = w.Pending(ctx, false); err != nil {
		return nil, err
	}
	b, _ := json.Marshal(map[string]any{"status": "closed", "pending": false, "hosts": len(observed), "source": f.latest.Sources[probeapi.FamilyHost]})
	return b, nil
}

func (f *firmwareRun) settleAttempts(ctx context.Context, w *writer.Writer, attempts []*firmwareAttempt, closing bool, end time.Time) error {
	for _, a := range attempts {
		if !a.Settled && a.Outcome.Kind != writer.Failed {
			if err := f.awaitBounds(ctx, w, a, closing, end); err != nil {
				return err
			}
		}
	}
	return nil
}

func (f *firmwareRun) active(ctx context.Context, w *writer.Writer) error {
	if err := w.Check(ctx); err != nil {
		return err
	}
	if !f.cleaning {
		select {
		case <-f.stopped:
			return errSafetyStop
		default:
		}
	}
	return nil
}
