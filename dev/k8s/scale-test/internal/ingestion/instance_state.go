// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
)

// A snapshot contains complete immutable families, never a truncated census.
type instanceSnapshot struct {
	unstable map[string]bool
	expected map[string]string
	bindings map[string]string
	rest     map[string]probeapi.Unit
	rows     map[probeapi.Family]map[string]probeapi.Unit
	meta     map[probeapi.Family]probeapi.SweepInfo
	config   probeapi.ConfigResponse
}

func (s *instanceSnapshot) oldestSource() time.Time {
	oldest := minTime(s.config.Rest.ReadStart.Time, s.config.RestSite.ReadStart.Time)
	for _, source := range s.meta {
		oldest = minTime(oldest, source.SweepStart.Time)
	}
	return oldest
}

var instanceFamilies = []struct {
	family probeapi.Family
	filter probeapi.Filter
}{
	{probeapi.FamilyHost, probeapi.Filter{Type: probeapi.UnitTypeHost}},
	{probeapi.FamilyDpu, probeapi.Filter{Type: probeapi.UnitTypeDpu}},
	{probeapi.FamilyInstance, probeapi.Filter{Type: probeapi.UnitTypeInstance}},
	{probeapi.FamilyExpected, probeapi.Filter{Type: probeapi.UnitTypeExpectedRecord, RecordType: new(probeapi.RecordTypeHost)}},
	{probeapi.FamilyRestMachines, probeapi.Filter{Type: probeapi.UnitTypeHost, Source: new(probeapi.SourceRest)}},
	{probeapi.FamilyRestInstances, probeapi.Filter{Type: probeapi.UnitTypeInstance, Source: new(probeapi.SourceRest)}},
}

func readInstanceSnapshot(ctx context.Context, p probe.Probe, until time.Time, s InstanceSettings, previous ...*instanceSnapshot) (*instanceSnapshot, error) {
	out := &instanceSnapshot{rows: map[probeapi.Family]map[string]probeapi.Unit{}, meta: map[probeapi.Family]probeapi.SweepInfo{}}
	cfg, err := p.Config(ctx, probe.ConfigFilter{InstanceType: &s.Instance.InstanceTypeID})
	if err != nil {
		return nil, instanceObservationError(err)
	}
	if len(previous) > 0 && previous[0] != nil && cfg.Metrics != nil && previous[0].config.Metrics != nil && cfg.Metrics.ProcessStartSeconds != previous[0].config.Metrics.ProcessStartSeconds {
		return nil, instanceIssue("native probe process changed between snapshots")
	}
	out.config = cfg
	interval := float64(s.Observation.IntervalSeconds)
	if cfg.Metrics == nil || cfg.Rest == nil || cfg.Rest.TenantAllocation == nil || cfg.RestSite == nil || cfg.RestSite.Result != probeapi.SiteMatchMatch {
		return nil, instanceIssue("probe lacks tenant allocation or a matching REST site")
	}
	allocation := cfg.Rest.TenantAllocation
	validRead := func(r probeapi.ReadInfo) bool {
		return r.LastAttemptError == nil && r.ReadIntervalSeconds == interval && r.ReadStart.Before(r.ReadAt.Time) && !r.ReadAt.After(time.Now()) && r.ReadAt.Sub(r.ReadStart.Time).Seconds() <= .75*interval && time.Since(r.ReadStart.Time) >= 0 && time.Since(r.ReadStart.Time).Seconds() <= 2*interval
	}
	if !validRead(cfg.Rest.ReadInfo) || !validRead(cfg.RestSite.ReadInfo) || allocation.Tenant != s.Instance.TenantID || allocation.InstanceType != s.Instance.InstanceTypeID {
		return nil, instanceIssue("allocation scope or source freshness is unproven")
	}
	for _, spec := range instanceFamilies {
		if len(previous) > 0 && previous[0] != nil {
			peek, err := p.Units(ctx, spec.filter, nil, probe.Page{Limit: new(uint32(1))})
			if err != nil {
				return nil, instanceObservationError(err)
			}
			if err = sourceMeta(peek.SweepInfo, spec.family, cfg.Metrics.ProcessStartSeconds, interval); err != nil {
				return nil, err
			}
			if peek.SnapshotID == previous[0].meta[spec.family].SnapshotID {
				out.rows[spec.family] = previous[0].rows[spec.family]
				out.meta[spec.family] = peek.SweepInfo
				continue
			}
		}
		rows, meta, err := probe.SnapshotUnits(ctx, p, until, spec.filter)
		if err != nil {
			return nil, instanceObservationError(err)
		}
		if err = sourceMeta(meta, spec.family, cfg.Metrics.ProcessStartSeconds, interval); err != nil {
			return nil, err
		}
		index := map[string]probeapi.Unit{}
		for _, u := range rows {
			source := probeapi.SourceNicoAPI
			if spec.filter.Source != nil {
				source = *spec.filter.Source
			}
			if u.ID == "" || u.Type != spec.filter.Type || u.Source != source {
				return nil, instanceIssue("unit scope is invalid")
			}
			if _, exists := index[u.ID]; exists {
				return nil, instanceIssue("duplicate unit identity")
			}
			index[u.ID] = u
		}
		if spec.filter.Source == nil {
			count, err := p.Count(ctx, spec.filter)
			if err != nil {
				return nil, instanceObservationError(err)
			}
			if count.SnapshotID != meta.SnapshotID || count.Count != uint64(len(rows)) || count.LastAttemptSucceeded == nil || !*count.LastAttemptSucceeded {
				return nil, instanceIssue("unit census disagrees with its source count")
			}
		}
		out.rows[spec.family], out.meta[spec.family] = index, meta
	}
	out.expected = map[string]string{}
	for _, id := range slices.Sorted(maps.Keys(out.rows[probeapi.FamilyExpected])) {
		u := out.rows[probeapi.FamilyExpected][id]
		if u.Expected != nil && u.Expected.DeviceID != nil {
			raw, _ := json.Marshal(u.Expected)
			out.expected[*u.Expected.DeviceID] += id + string(raw)
		}
	}
	out.bindings = map[string]string{}
	for _, id := range slices.Sorted(maps.Keys(out.rows[probeapi.FamilyInstance])) {
		u := out.rows[probeapi.FamilyInstance][id]
		if u.Instance != nil && u.Instance.HostID != nil {
			raw, _ := json.Marshal(u.Instance)
			out.bindings[*u.Instance.HostID] += string(u.Stage) + ":" + string(raw)
		}
	}
	_, err = out.restHosts()
	return out, err
}

func (s *instanceSnapshot) restHosts() (map[string]probeapi.Unit, error) {
	if s.rest != nil {
		return s.rest, nil
	}
	out := map[string]probeapi.Unit{}
	for _, u := range s.rows[probeapi.FamilyRestMachines] {
		if u.RestMachine == nil || u.RestMachine.ControllerID == "" || !safeMachineID(u.ID) {
			return nil, instanceIssue("REST host has no native identity")
		}
		id := u.RestMachine.ControllerID
		if _, ok := out[id]; ok {
			return nil, instanceIssue("ambiguous REST host identity")
		}
		out[id] = u
	}
	s.rest = out
	return out, nil
}

func readyHost(u probeapi.Unit) bool {
	return u.Stage == probeapi.StageReady && u.Host != nil && u.Host.Allocatable != nil && *u.Host.Allocatable
}
func restHealthy(u probeapi.Unit) bool {
	return u.RestMachine != nil && (u.State == "Ready" || u.State == "InUse")
}
func restFree(u probeapi.Unit, kind string) bool {
	return u.RestMachine != nil && u.State == "Ready" && u.RestMachine.IsUsableByTenant && u.RestMachine.InstanceType != nil && *u.RestMachine.InstanceType == kind && u.RestMachine.InstanceID == nil && u.RestMachine.TenantID == nil
}

// CohortHost is frozen before labels or instance writes. Outcome fields are
// appended only to the separate owned manifest after verified settlement.
type CohortHost struct {
	ExpectedRecord   string   `json:"expectedRecord"`
	MAC              string   `json:"mac"`
	DPUs             []string `json:"dpus"`
	Final            string   `json:"final,omitempty"`
	FirstFailure     string   `json:"firstFailure,omitempty"`
	ID               string   `json:"id"`
	RestID           string   `json:"restId"`
	Rack             string   `json:"rack"`
	Pod              string   `json:"pod"`
	PodUID           string   `json:"podUid"`
	BMC              string   `json:"bmc"`
	Burst            int      `json:"burst"`
	Initial          string   `json:"initial"`
	InstanceID       string   `json:"instanceId,omitempty"`
	NativeInstanceID string   `json:"nativeInstanceId,omitempty"`
	Name             string   `json:"name,omitempty"`
}
type instancePlan struct {
	Network      map[string]string `json:"network,omitempty"`
	sourceHash   string
	SourceOwned  map[string]string                      `json:"sourceOwned,omitempty"`
	CommandRunID string                                 `json:"runId"`
	Health       map[string]any                         `json:"health"`
	Inventory    map[string]string                      `json:"inventory"`
	Requests     []instanceRequest                      `json:"requests"`
	ScriptSHA    string                                 `json:"scriptSha256"`
	RunID        string                                 `json:"ownerRunId"`
	Tenant       string                                 `json:"tenant"`
	Type         string                                 `json:"type"`
	Org          string                                 `json:"org"`
	Origin       string                                 `json:"origin"`
	Cohort       []CohortHost                           `json:"cohort"`
	Excluded     map[string]string                      `json:"excluded"`
	Baseline     map[string]string                      `json:"baseline"`
	Sources      map[probeapi.Family]probeapi.SweepInfo `json:"sources"`
}

func planInstances(s *instanceSnapshot, inv inventory.SiteInventory, mat map[string]podState, settings InstanceSettings, runID, namespace string) (instancePlan, error) {
	plan := instancePlan{RunID: runID, Tenant: settings.Instance.TenantID, Type: settings.Instance.InstanceTypeID, Org: settings.Rest.Org, Origin: settings.Rest.URL, Excluded: map[string]string{}, Baseline: map[string]string{}, Sources: s.meta}
	rest, err := s.restHosts()
	if err != nil {
		return plan, err
	}
	type owner struct {
		record, mac string
		pod, rack   string
		healthy     bool
		dpus        int
	}
	owners := map[string]owner{}
	for _, p := range inv.Pods {
		if p.UID == "" || p.UID != mat[namespace+"/"+p.Name].UID {
			return plan, instanceIssue("MAT records and pod identity do not agree")
		}
		dpuCounts := map[string]int{}
		for _, dpu := range p.Records.DPUs {
			dpuCounts[strings.ToLower(dpu.ParentBmcMac)]++
		}
		for _, d := range p.Records.Devices {
			if d.Kind != inventory.KindMachine {
				continue
			}
			mac := strings.ToLower(d.BmcMac)
			if _, ok := owners[mac]; ok {
				return plan, instanceIssue("ambiguous MAT host")
			}
			rack := ""
			if d.RackID != nil {
				rack = *d.RackID
			}
			o := owner{pod: p.Name, rack: rack, healthy: p.Ready}
			o.dpus = dpuCounts[mac]
			owners[mac] = o
		}
	}
	linked := map[string]owner{}
	for _, u := range s.rows[probeapi.FamilyExpected] {
		e := u.Expected
		if e == nil || e.BmcMac == nil {
			return plan, instanceIssue("host record lacks MAT identity")
		}
		mac := strings.ToLower(*e.BmcMac)
		o, ok := owners[mac]
		if !ok {
			return plan, instanceIssue("unexpected host record")
		}
		delete(owners, mac)
		if !e.Linked || e.DeviceID == nil {
			plan.Excluded["expected:"+mac] = "expected host has no native link"
			continue
		}
		if _, ok := linked[*e.DeviceID]; ok {
			return plan, instanceIssue("ambiguous host link")
		}
		o.record, o.mac = u.ID, mac
		linked[*e.DeviceID] = o
	}
	for mac := range owners {
		plan.Excluded["expected:"+mac] = "expected host record is missing"
	}
	for id := range linked {
		if s.rows[probeapi.FamilyHost][id].ID == "" {
			plan.Excluded[id] = "record-linked host is missing"
			plan.Baseline[id] = hostSignature(s, id)
		}
	}
	for id, u := range s.rows[probeapi.FamilyDpu] {
		if u.Dpu == nil || u.Dpu.HostID == nil {
			plan.Excluded["dpu:"+id] = "unattached DPU, initial state " + u.State
		}
	}
	eligible := []CohortHost{}
	for _, id := range slices.Sorted(maps.Keys(s.rows[probeapi.FamilyHost])) {
		u := s.rows[probeapi.FamilyHost][id]
		r := rest[id]
		o, linked := linked[id]
		if !linked {
			return plan, instanceIssue("native host has no fixture provenance")
		}
		reason := ""
		labelUsed := false
		if r.RestMachine != nil {
			_, labelUsed = r.RestMachine.Labels[instanceLabel]
		}
		switch {
		case s.unstable[id]:
			reason = "host changed during calibration"
		case !o.healthy:
			reason = "MAT pod is not Ready"
		case labelUsed:
			reason = "host has a prior selector owner"
		case !readyHost(u):
			reason = "host is not Ready and allocatable"
		case !restFree(r, settings.Instance.InstanceTypeID):
			reason = "REST host is not free and eligible"
		case !linked || o.rack == "" || u.Host.RackID == nil || *u.Host.RackID != o.rack || u.BmcIP == nil || *u.BmcIP == "":
			reason = "host fixture identity is incomplete"
		case o.dpus == 0 || len(u.Host.DpuIDs) != o.dpus || len(slices.Compact(slices.Sorted(slices.Values(u.Host.DpuIDs)))) != len(u.Host.DpuIDs):
			reason = "provisioned DPU links are incomplete"
		}
		if reason == "" {
			for _, d := range u.Host.DpuIDs {
				du, ok := s.rows[probeapi.FamilyDpu][d]
				if !ok || du.Stage != probeapi.StageReady || du.Dpu == nil || du.Dpu.HostID == nil || *du.Dpu.HostID != id {
					reason = "DPU dependency is not Ready"
				}
			}
		}
		pod, ok := mat[namespace+"/"+o.pod]
		if !ok || pod.UID == "" {
			reason = "MAT pod identity is missing"
		}
		plan.Baseline[id] = hostSignature(s, id)
		if reason != "" {
			plan.Excluded[id] = reason
			continue
		}
		eligible = append(eligible, CohortHost{ExpectedRecord: o.record, MAC: o.mac, ID: id, RestID: r.ID, Rack: o.rack, Pod: o.pod, PodUID: pod.UID, BMC: *u.BmcIP, Initial: plan.Baseline[id], DPUs: slices.Sorted(slices.Values(u.Host.DpuIDs))})
	}
	if uint64(len(eligible)) < settings.Instance.N {
		return plan, instanceIssue("requested N exceeds the eligible cohort")
	}
	plan.Cohort = slices.Clone(eligible[:settings.Instance.N])
	sizes, err := batches(settings.Instance.N, settings.Instance.BurstSize)
	if err != nil {
		return plan, err
	}
	offset := 0
	for burst, n := range sizes {
		for i := offset; i < offset+n; i++ {
			plan.Cohort[i].Burst = burst
		}
		offset += n
	}
	return plan, nil
}

// Only lifecycle, allocation and dependency identity affect transitions.
func hostSignature(s *instanceSnapshot, id string) string {
	u := s.rows[probeapi.FamilyHost][id]
	v := struct {
		State          string
		Stage          probeapi.Stage
		Allocatable    *bool
		Rest           string
		BMC            *string
		Rack           *string
		RESTID         string
		RESTType       *string
		RESTTenant     *string
		RESTUsable     bool
		Selector       string
		SelectorOwned  bool
		DPUList        []string
		Binding        *string
		Expected       string
		NativeInstance string
		DPUs           map[string]string
	}{BMC: u.BmcIP, State: u.State, Stage: u.Stage, NativeInstance: s.bindings[id], Expected: s.expected[id], DPUs: map[string]string{}}
	if u.Host != nil {
		v.Rack = u.Host.RackID
		v.DPUList = slices.Sorted(slices.Values(u.Host.DpuIDs))
		v.Allocatable = u.Host.Allocatable
		for _, id := range u.Host.DpuIDs {
			d := s.rows[probeapi.FamilyDpu][id]
			host := ""
			if d.Dpu != nil && d.Dpu.HostID != nil {
				host = *d.Dpu.HostID
			}
			v.DPUs[id] = string(d.Stage) + ":" + d.State + ":" + host
		}
	}
	if r := s.rest[id]; r.RestMachine != nil {
		v.RESTID = r.ID
		v.RESTType = r.RestMachine.InstanceType
		v.RESTTenant = r.RestMachine.TenantID
		v.RESTUsable = r.RestMachine.IsUsableByTenant
		v.Selector, v.SelectorOwned = r.RestMachine.Labels[instanceLabel]
		v.Rest = r.State
		v.Binding = r.RestMachine.InstanceID
	}
	b, _ := json.Marshal(v)
	return string(b)
}

// Evidence is bounded on disk, synced before another dispatch, and hashed.
type instanceEvidence struct {
	file  *os.File
	path  string
	bytes int
	count int
	hash  string
}

func openEvidence(dir, runID string) (*instanceEvidence, error) {
	path := filepath.Join(dir, runID+".transitions.jsonl")
	f, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
	if err != nil {
		return nil, err
	}
	d, e := os.Open(dir)
	if e != nil {
		_ = f.Close()
		return nil, e
	}
	e = errors.Join(f.Sync(), d.Sync(), d.Close())
	if e != nil {
		_ = f.Close()
		return nil, e
	}
	return &instanceEvidence{file: f, path: path}, nil
}
func (e *instanceEvidence) append(v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	if e.bytes+len(b)+1 > 256<<20 {
		return errors.New("transition evidence quota exhausted")
	}
	b = append(b, '\n')
	if _, err = e.file.Write(b); err != nil {
		return err
	}
	if err = e.file.Sync(); err != nil {
		return err
	}
	e.bytes += len(b)
	e.count++
	return nil
}
func (e *instanceEvidence) finish() error {
	if e.file == nil {
		return nil
	}
	if _, err := e.file.Seek(0, 0); err != nil {
		return err
	}
	h := sha256.New()
	if _, err := io.Copy(h, e.file); err != nil {
		return err
	}
	e.hash = hex.EncodeToString(h.Sum(nil))
	f := e.file
	e.file = nil
	return f.Close()
}
func writeOnce(dir, name string, v any) (string, error) {
	b, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	if len(b) > 128<<20 {
		return "", errors.New("plan exceeds evidence quota")
	}
	p := filepath.Join(dir, name)
	f, err := os.OpenFile(p, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return "", err
	}
	_, writeErr := f.Write(b)
	err = errors.Join(writeErr, f.Sync(), f.Close())
	if err != nil {
		return "", err
	}
	d, err := os.Open(dir)
	if err != nil {
		return "", err
	}
	err = errors.Join(d.Sync(), d.Close())
	return digest(b), err
}
func readOwned(dir, name string) (instancePlan, error) {
	var plan instancePlan
	if filepath.Base(name) != name || !strings.HasSuffix(name, ".owned.json") {
		return plan, instanceIssue("owned manifest must be a basename in the shared run directory")
	}
	f, err := os.Open(filepath.Join(dir, name))
	if err != nil {
		return plan, instanceIssue("owned manifest unavailable")
	}
	defer func() { _ = f.Close() }()
	b, err := io.ReadAll(io.LimitReader(f, (128<<20)+1))
	if err != nil || len(b) > 128<<20 || json.Unmarshal(b, &plan) != nil {
		return plan, instanceIssue("owned manifest is incomplete or over its quota")
	}
	plan.sourceHash = digest(b)
	return plan, nil
}

// transition is a change since the previous successful observation, never a
// refreshed baseline. Background attribution remains explicitly undetermined.
type transition struct {
	Observation   int                                    `json:"observation"`
	Host          string                                 `json:"host"`
	Class         string                                 `json:"class"`
	Burst         int                                    `json:"burst"`
	Before        string                                 `json:"before"`
	After         string                                 `json:"after"`
	Source        probeapi.SweepInfo                     `json:"source"`
	BeforeSource  *probeapi.SweepInfo                    `json:"beforeSource,omitempty"`
	Sources       map[probeapi.Family]probeapi.SweepInfo `json:"sources,omitempty"`
	BeforeSources map[probeapi.Family]probeapi.SweepInfo `json:"beforeSources,omitempty"`
	Attribution   string                                 `json:"attribution"`
}

type instanceDetail struct {
	Checks            []readiness.Check                      `json:"checks"`
	Cohort            any                                    `json:"cohort"`
	CohortTimeline    []transition                           `json:"cohortTimeline"`
	BackgroundChanges []transition                           `json:"backgroundChanges"`
	Evidence          map[string]any                         `json:"evidence"`
	Bursts            []any                                  `json:"bursts"`
	Manifest          string                                 `json:"ownedManifest,omitempty"`
	Calibration       map[probeapi.Family]probeapi.SweepInfo `json:"calibration"`
}

func (d instanceDetail) outcome(v result.Verdict, err error) result.Outcome {
	b, _ := json.Marshal(d)
	reasons := []string{}
	if err != nil {
		reasons = append(reasons, cut(err.Error()))
	}
	return result.Found(v, reasons, b)
}

func sourceMeta(meta probeapi.SweepInfo, family probeapi.Family, epoch, interval float64) error {
	if meta.SweepIntervalSeconds != interval {
		return instanceIssue(fmt.Sprintf("probe %s cadence is %g seconds but runner.observation.intervalSeconds is %g", family, meta.SweepIntervalSeconds, interval))
	}
	prefix, _, ok := strings.Cut(meta.SnapshotID, "-")
	process, err := strconv.ParseUint(prefix, 10, 64)
	gap := epoch - float64(process)/1000
	if !ok || err != nil || !(gap >= -0.000001 && gap < .001001) || meta.LastAttemptSucceeded == nil || !*meta.LastAttemptSucceeded || meta.Family != family || !meta.SweepStart.Before(meta.ReadAt.Time) || meta.ReadAt.After(time.Now()) || time.Since(meta.SweepStart.Time) < 0 || time.Since(meta.SweepStart.Time).Seconds() > 2*interval || meta.ReadAt.Sub(meta.SweepStart.Time).Seconds() > .75*interval {
		return instanceIssue("source epoch, cadence, latest attempt or sweep bounds changed")
	}
	return nil
}

// Machine IDs are opaque, but must be safe as a single unescaped URL segment.
func safeMachineID(id string) bool {
	if id == "" || id == "." || id == ".." || len(id) > 512 {
		return false
	}
	for _, c := range id {
		safe := c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || strings.ContainsRune("-._~", c)
		if !safe {
			return false
		}
	}
	return true
}
