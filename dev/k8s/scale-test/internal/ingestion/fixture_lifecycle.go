// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/google/uuid"
)

func fixtureFile(dir, name string, v any, hash string) error {
	if filepath.Base(name) != name {
		return instanceIssue("fixture manifest path invalid")
	}
	b, err := fixtureBytes(filepath.Join(dir, name), 128<<20)
	if err != nil {
		return err
	}
	if hash != "" && digest(b) != hash || decodeFixture(b, v) != nil {
		return instanceIssue("fixture ownership manifest invalid or unsupported; operator reconciliation required")
	}
	return nil
}
func (f *fixtureRun) load() error {
	f.loaded = false
	f.previous = map[string]fixtureRecord{}
	var ref map[string]string
	err := fixtureFile(f.in.t.cfg.Journal.Dir, f.in.t.s.Fixture.Name+".fixture-owner.json", &ref, "")
	if os.IsNotExist(err) {
		f.loaded = true
		return nil
	}
	if err != nil {
		return err
	}
	var old fixtureLedger
	if err = fixtureFile(f.in.t.cfg.Journal.Dir, ref["path"], &old, ref["sha256"]); err != nil {
		return err
	}
	if err = f.acceptLedger(old); err != nil {
		return err
	}
	f.imported = &old
	f.ledger.Records = slices.Clone(old.Records)
	f.loaded = true
	return nil
}
func (f *fixtureRun) acceptLedger(old fixtureLedger) error {
	s := f.in.t.s
	if old.Name != s.Fixture.Name || old.Site != s.Fixture.SiteID || old.Org != s.Rest.Org || old.Provider != s.Rest.ProviderOrg || old.Origin != s.Rest.URL || old.TenantClient != s.Rest.Auth.TenantClientID {
		return instanceIssue("fixture ownership belongs to another scope")
	}
	for _, r := range old.Records {
		path, known := fixturePaths[r.Kind]
		provider := r.Kind == "account" || r.Kind == "type" || r.Kind == "compute" || r.Kind == "ipallocation" || r.Kind == "ipblock"
		if strings.HasPrefix(r.Kind, "association:") {
			known = true
			provider = true
			path = "instance/type/" + text(r.Spec, "instanceTypeId") + "/machine"
			if uuid.Validate(text(r.Spec, "instanceTypeId")) != nil || r.Kind != "association:"+r.ID {
				return instanceIssue("fixture association proof invalid")
			}
		}
		if !known || (!strings.HasPrefix(r.Kind, "association:") && uuid.Validate(r.ID) != nil || strings.HasPrefix(r.Kind, "association:") && !safeMachineID(r.ID)) || r.Path != path+"/"+r.ID || r.Provider != provider {
			return instanceIssue("fixture ownership path or scope invalid")
		}
		if r.Owned && r.Proof == 0 {
			return instanceIssue("fixture ownership lacks mutation proof")
		}
		f.previous[r.Kind] = r
	}
	return nil
}
func (in *instanceRun) beginFixture(inv inventory.SiteInventory) (*fixtureRun, error) {
	s := in.t.s
	f := &fixtureRun{in: in, p: s.sidecar, epoch: s.restEpoch, end: minTime(in.end, time.Now().Add(time.Duration(s.Fixture.SetupSeconds)*time.Second)), report: map[string]any{"setup": map[string]any{"verdict": "running", "startedAt": time.Now()}, "measurement": map[string]string{"verdict": "notReady"}, "teardown": map[string]string{"verdict": "skipped"}}}
	f.ledger = fixtureLedger{Name: s.Fixture.Name, Site: s.Fixture.SiteID, Org: s.Rest.Org, Provider: s.Rest.ProviderOrg, Origin: s.Rest.URL, RunID: in.t.journalID(), TenantClient: s.Rest.Auth.TenantClientID, Cohort: map[string]string{}, Records: []fixtureRecord{}}
	ctx, cancel := context.WithDeadline(in.ctx, f.end)
	defer cancel()
	in.d.Evidence["fixture"] = f.report
	if err := fabricPrerequisite(s.Fixture); err != nil {
		return f, err
	}
	id, err := identityFrom(s.Rest.Auth.ProviderDirectory)
	if err != nil {
		return f, err
	}
	if err = writeIdentity(s.Rest.Auth.CredentialsDir, "provider", id); err != nil {
		return f, err
	}
	// Wait for provider publication only. Tenant readiness cannot gate startup.
	for {
		page, e := s.sidecar.Units(ctx, probeapi.Filter{Type: probeapi.UnitTypeHost, Source: new(probeapi.SourceRest)}, nil, probe.Page{Limit: new(uint32(1))})
		if e == nil && page.LastAttemptSucceeded != nil && *page.LastAttemptSucceeded {
			break
		}
		if !time.Now().Before(f.end) {
			return f, instanceIssue("provider Machine census did not become available")
		}
		if err = waitInstance(ctx, 10); err != nil {
			return f, err
		}
	}
	providers, e := f.lookup("provider")
	if e != nil || len(providers) != 1 {
		return f, errors.Join(e, instanceIssue("provider identity unavailable"))
	}
	f.provider = text(providers[0], "id")
	types, err := f.lookup("type")
	if err != nil {
		return f, err
	}
	if len(types) > 0 {
		f.kind = text(types[0], "id")
		if err = f.scope("type", types[0]); err != nil {
			return f, err
		}
	}
	in.t.s.restEpoch = f.epoch
	if err = in.calibrate(f.end); err != nil {
		return f, err
	}
	snap := in.latest
	in.epoch = snap.config.Metrics.ProcessStartSeconds
	for _, u := range snap.rows[probeapi.FamilyInstance] {
		if u.Instance == nil || u.Instance.Tenant == nil || *u.Instance.Tenant != s.Rest.Org {
			return f, instanceIssue("other-tenant instances are outside the fixture contract")
		}
	}
	if in.deleting {
		in.plan, err = readOwned(in.t.cfg.Journal.Dir, s.Instance.OwnedManifest)
		if err != nil {
			return f, err
		}
		ref := in.plan.Fixture
		var ledger fixtureLedger
		if ref == nil {
			return f, instanceIssue("owned instance manifest has no fixture provenance")
		}
		if err = fixtureFile(in.t.cfg.Journal.Dir, ref["path"], &ledger, ref["sha256"]); err != nil {
			return f, err
		}
		f.previous = map[string]fixtureRecord{}
		if err = f.acceptLedger(ledger); err != nil {
			return f, err
		}
		f.ledger = ledger
		for _, h := range in.plan.Cohort {
			u := snap.rows[probeapi.FamilyHost][h.ID]
			r := snap.rest[h.ID]
			if u.Host == nil || u.BmcIP == nil || *u.BmcIP != h.BMC || r.RestMachine == nil || r.ID != h.RestID || !in.absent(h) && (r.RestMachine.InstanceID == nil || *r.RestMachine.InstanceID != h.InstanceID) {
				return f, instanceIssue("original fixture cohort identity changed")
			}
		}
		f.tenant = in.plan.Tenant
		in.t.s.Instance.TenantID, in.t.s.Instance.InstanceTypeID = in.plan.Tenant, in.plan.Type
	} else {
		planned := *snap
		planned.rest = maps.Clone(snap.rest)
		selectedType := f.kind
		if selectedType == "" {
			selectedType = uuid.Nil.String()
		}
		settings := s
		settings.Instance.InstanceTypeID = selectedType
		for id, r := range planned.rest {
			m := *r.RestMachine
			compatible := m.InstanceType == nil || *m.InstanceType == f.kind
			compatible = compatible && capabilitiesMatch(m.MachineCapabilities, s.Fixture.MachineCapabilities)
			if compatible && m.InstanceID == nil && m.TenantID == nil {
				m.InstanceType = new(selectedType)
				m.IsUsableByTenant = true
			} else {
				m.IsUsableByTenant = false
			}
			r.RestMachine = &m
			planned.rest[id] = r
		}
		in.plan, err = planInstances(&planned, inv, in.mat, settings, in.t.cfg.RunID, in.t.cfg.Namespace)
		if err != nil {
			return f, err
		}
	}
	for id := range snap.rows[probeapi.FamilyHost] {
		in.plan.Baseline[id] = hostSignature(snap, id)
	}
	for i, h := range in.plan.Cohort {
		key := in.t.cfg.Namespace + "/" + h.Pod
		if !slices.Contains(in.selectedPods, key) {
			in.selectedPods = append(in.selectedPods, key)
		}
		in.plan.Cohort[i].Initial = in.plan.Baseline[h.ID]
		f.ledger.Cohort[h.ID] = h.RestID
	}
	file := in.t.cfg.RunID + ".fixture-cohort.json"
	hash, err := writeOnce(in.t.cfg.Journal.Dir, file, in.plan)
	if err != nil {
		return f, err
	}
	f.report["cohort"] = map[string]string{"path": file, "sha256": hash}
	if err = in.w.Record(in.ctx, "fixtureCohort", f.report["cohort"]); err != nil {
		return f, err
	}
	return f, nil
}
func (f *fixtureRun) associate() error {
	ctx, cancel := context.WithDeadline(f.in.ctx, f.end)
	defer cancel()
	in := f.in
	var todo []CohortHost
	seen := map[string]bool{}
	for _, r := range f.ledger.Records {
		seen[r.Kind] = true
	}
	for _, r := range f.previous {
		if strings.HasPrefix(r.Kind, "association:") && r.Owned && !seen[r.Kind] {
			f.ledger.Records = append(f.ledger.Records, r)
		}
	}
	for _, h := range in.plan.Cohort {
		r := in.latest.rest[h.ID].RestMachine
		if r.InstanceType != nil {
			if *r.InstanceType != f.kind {
				return instanceIssue("selected host has a foreign type")
			}
			continue
		}
		todo = append(todo, h)
	}
	for start := 0; start < len(todo); start += 100 {
		chunk := todo[start:min(start+100, len(todo))]
		ids := []string{}
		for _, h := range chunk {
			ids = append(ids, h.RestID)
		}
		out, err := f.mutate(http.MethodPost, "instance/type/"+f.kind+"/machine", map[string]any{"machineIds": ids}, true, 0, chunk)
		if err != nil {
			// Read back once within the setup bound. Uncertain associations
			// remain pending even when visible and are never adopted or replayed.
			_, _, readErr := probe.SnapshotUnits(ctx, f.p, minTime(f.end, time.Now().Add(300*time.Second)), probeapi.Filter{Type: probeapi.UnitTypeHost, Source: new(probeapi.SourceRest)})
			return errors.Join(err, readErr)
		}
		for _, h := range chunk {
			r := fixtureRecord{Kind: "association:" + h.RestID, ID: h.RestID, Path: "instance/type/" + f.kind + "/machine/" + h.RestID, Provider: true, Owned: true, Spec: map[string]any{"hostId": h.ID, "instanceTypeId": f.kind}, Proof: out.Op, RunID: in.t.journalID()}
			f.ledger.Records = append(f.ledger.Records, r)
			if err = in.w.Record(in.ctx, "fixtureResource", r); err != nil {
				return err
			}
		}
		if err = f.checkpoint(); err != nil {
			return err
		}
	}
	if len(todo) == 0 {
		return nil
	}
	_, err := f.pollMachines(time.Now(), func(rows map[string]probeapi.Unit, _ probeapi.SweepInfo) (bool, error) {
		for _, h := range todo {
			m := rows[h.RestID].RestMachine
			if m == nil || m.InstanceType == nil || *m.InstanceType != f.kind {
				return false, nil
			}
		}
		return true, nil
	})
	return err
}

func (f *fixtureRun) finishSetup(err error) error {
	verdict := "passed"
	if f.in.rc.DryRun {
		verdict = "planned"
	}
	if err != nil {
		f.report["measurement"] = map[string]string{"verdict": "skipped"}
		verdict = "unresolved"
	}
	f.report["setup"] = map[string]any{"verdict": verdict, "finishedAt": time.Now()}
	return errors.Join(err, f.checkpoint())
}
func (f *fixtureRun) checkpoint() error {
	f.checkpoints++
	file := fmt.Sprintf("%s.fixture-%d.json", f.in.t.cfg.RunID, f.checkpoints)
	hash, err := writeOnce(f.in.t.cfg.Journal.Dir, file, f.ledger)
	if err != nil {
		return err
	}
	ref := map[string]string{"path": file, "sha256": hash}
	f.report["manifest"] = ref
	if !f.in.deleting {
		f.in.plan.Fixture = ref
	}
	if err = f.in.w.Record(f.in.ctx, "fixtureManifest", ref); err != nil {
		return err
	}
	if !f.loaded || f.in.rc.DryRun {
		return nil
	}
	return f.commitOwner()
}

func (f *fixtureRun) commitOwner() error {
	if f.in.deleting {
		return nil
	}
	ref, ok := f.report["manifest"].(map[string]string)
	if !ok {
		return instanceIssue("fixture manifest unavailable")
	}
	temp := f.in.t.cfg.RunID + ".fixture-index.tmp"
	if _, err := writeOnce(f.in.t.cfg.Journal.Dir, temp, ref); err != nil {
		return err
	}
	if err := f.in.w.Record(f.in.ctx, "fixtureOwner", ref); err != nil {
		return err
	}
	if err := os.Rename(filepath.Join(f.in.t.cfg.Journal.Dir, temp), filepath.Join(f.in.t.cfg.Journal.Dir, f.in.t.s.Fixture.Name+".fixture-owner.json")); err != nil {
		return err
	}
	return syncFixtureDir(f.in.t.cfg.Journal.Dir)
}
func (f *fixtureRun) reuse() error {
	if err := f.tenantIdentity(); err != nil {
		return err
	}
	for _, r := range f.ledger.Records {
		if _, ok := fixturePaths[r.Kind]; !ok {
			continue
		}
		kind := r.Kind
		if kind == "ipblock" && !r.Owned {
			kind = "providerBlocks"
		}
		rows, err := f.lookup(kind)
		if kind == "providerBlocks" {
			rows = slices.DeleteFunc(rows, func(row map[string]any) bool { return text(row, "id") != r.ID })
		}
		if err != nil {
			return err
		}
		switch r.Kind {
		case "vpc":
			f.in.t.s.Instance.VPCID = r.ID
		case "subnet":
			f.in.t.s.Instance.SubnetID = r.ID
		case "os":
			f.in.t.s.Instance.OperatingSystemID = r.ID
		}
		if r.Owned && len(rows) == 0 {
			continue
		}
		if len(rows) != 1 || text(rows[0], "id") != r.ID || !fixtureSpec(r.Kind, r.Spec, rows[0]) {
			return instanceIssue("original fixture identity or specification changed")
		}
	}
	return nil
}
func (f *fixtureRun) teardown() error {
	in := f.in
	f.end = minTime(in.end, time.Now().Add(time.Duration(in.t.s.Fixture.SetupSeconds)*time.Second))
	f.report["teardown"] = map[string]string{"verdict": "unresolved"}
	original := f.ledger
	if err := f.load(); err != nil {
		return err
	}
	if f.imported != nil {
		current := map[string]string{}
		for _, r := range f.imported.Records {
			current[r.Kind] = r.ID
		}
		for _, r := range original.Records {
			if _, known := fixturePaths[r.Kind]; known && current[r.Kind] != r.ID {
				return instanceIssue("fixture ownership generation changed")
			}
		}
		f.ledger = *f.imported
	}
	f.cleanupHosts = map[string]bool{}
	for _, r := range f.ledger.Records {
		if r.Owned && strings.HasPrefix(r.Kind, "association:") {
			f.cleanupHosts[text(r.Spec, "hostId")] = true
		}
	}

	if len(in.latest.rows[probeapi.FamilyInstance]) != 0 || len(in.latest.rows[probeapi.FamilyRestInstances]) != 0 {
		return instanceIssue("fixture teardown requires an empty native and REST tenant")
	}
	if err := f.beforeWrite(); err != nil {
		return err
	}
	for _, record := range f.ledger.Records {
		if !record.Owned {
			continue
		}
		if record.Kind == "type" {
			for id, unit := range in.latest.rest {
				if unit.RestMachine.InstanceType != nil && *unit.RestMachine.InstanceType == record.ID && !f.cleanupHosts[id] {
					return instanceIssue("owned type still has borrowed host associations")
				}
			}
		}
		if _, known := fixturePaths[record.Kind]; !known {
			continue
		}
		rows, err := f.lookup(record.Kind)
		if err != nil {
			return err
		}
		if len(rows) > 0 && (text(rows[0], "id") != record.ID || f.scope(record.Kind, rows[0]) != nil || !fixtureSpec(record.Kind, record.Spec, rows[0])) {
			return instanceIssue("fixture teardown ownership or spec changed")
		}
	}
	var detached []probeapi.Unit
	var detachAfter time.Time
	// Reverse creation order removes subnet/OS/VPC before blocks and quotas.
	for _, r := range slices.Backward(f.ledger.Records) {
		if r.Kind == "type" && (r.Owned || !detachAfter.IsZero()) {
			var e error
			detached, e = f.awaitDetached(detachAfter)
			if e != nil {
				return e
			}
		}
		if r.Kind == "type" && r.Owned {
			for _, unit := range detached {
				if unit.RestMachine != nil && unit.RestMachine.InstanceType != nil && *unit.RestMachine.InstanceType == r.ID {
					return instanceIssue("owned type still has borrowed host associations")
				}
			}
		}
		if !r.Owned {
			continue
		}
		if strings.HasPrefix(r.Kind, "association:") {
			host := text(r.Spec, "hostId")
			m := in.latest.rest[host].RestMachine
			if !readyHost(in.latest.rows[probeapi.FamilyHost][host]) || m == nil || m.InstanceID != nil || m.TenantID != nil {
				return instanceIssue("owned association host is not free")
			}
			if m.InstanceType != nil {
				out, e := f.mutate(http.MethodDelete, r.Path, nil, true, 0, r)
				if e != nil {
					return e
				}
				detachAfter = out.RepliedAt
			}
			continue
		}
		rows, err := f.lookup(r.Kind)
		if err != nil {
			return err
		}
		if len(rows) == 0 {
			continue
		}
		if text(rows[0], "id") != r.ID || !fixtureSpec(r.Kind, r.Spec, rows[0]) {
			return instanceIssue("fixture teardown ownership or spec changed")
		}
		if _, err = f.mutate(http.MethodDelete, r.Path, nil, r.Provider, 0, rows[0], f.unchanged(r.Kind, rows[0])); err != nil {
			return err
		}
		if _, err = f.wait(r.Kind, func(rows []map[string]any) bool { return len(rows) == 0 }); err != nil {
			return err
		}
	}
	if err := in.w.Record(in.ctx, "fixtureRestFinalized", map[string]any{"finishedAt": time.Now().UTC().Format(time.RFC3339Nano), "manifest": in.plan.Fixture}); err != nil {
		return err
	}
	if err := in.evidence.finish(); err != nil {
		return err
	}
	in.safetyMu.Lock()
	in.fixtureRESTClosed = true
	in.safetyMu.Unlock()
	if err := in.w.Record(in.ctx, "fixtureOwnerRetired", map[string]string{"name": in.t.s.Fixture.Name}); err != nil {
		return err
	}
	if err := os.Remove(filepath.Join(in.t.cfg.Journal.Dir, in.t.s.Fixture.Name+".fixture-owner.json")); err != nil && !os.IsNotExist(err) {
		return err
	}
	if err := syncFixtureDir(in.t.cfg.Journal.Dir); err != nil {
		return err
	}
	f.report["teardown"] = map[string]any{"verdict": "passed", "residue": []string{"tenant row retained: REST has no tenant DELETE", "borrowed fixture objects and grants retained"}}
	return nil
}

func (f *fixtureRun) beforeWrite() error {
	if err := f.in.safety(f.end); err != nil {
		return err
	}
	if f.in.fixtureRESTClosed {
		return nil
	}
	ctx, cancel := context.WithDeadline(f.in.ctx, f.end)
	defer cancel()
	settings := f.in.t.s
	settings.preparing = true
	settings.tenantCensus = f.cleanupHosts != nil
	next, err := readInstanceSnapshot(ctx, f.in.t.p, f.end, settings, f.in.latest)
	if err != nil {
		return err
	}
	if settings.tenantCensus && (len(next.rows[probeapi.FamilyRestInstances]) != 0 || len(next.rows[probeapi.FamilyInstance]) != 0) {
		return instanceIssue("fixture teardown requires a current empty native and REST tenant")
	}
	if len(next.rows[probeapi.FamilyInstance]) != len(f.in.latest.rows[probeapi.FamilyInstance]) {
		return instanceIssue("instance census changed during fixture work")
	}
	for id, before := range f.in.plan.Baseline {
		if _, selected := f.ledger.Cohort[id]; !selected {
			after := hostSignature(next, id)
			if f.cleanupHosts[id] {
				before = withoutAssociation(before)
				after = withoutAssociation(after)
			}
			if err = f.background(id, before, after, next); err != nil {
				return err
			}
		}
	}
	if err = f.checkFrozen(next); err != nil {
		return err
	}
	f.in.latest = next
	return nil
}

func capabilitiesMatch(have, want []probeapi.MachineCapability) bool {
	for _, w := range want {
		matches := 0
		for _, h := range have {
			if h.Type == w.Type && h.Name == w.Name && (w.Count == nil || h.Count != nil && *h.Count == *w.Count) {
				matches++
			}
		}
		if matches != 1 {
			return false
		}
	}
	return true
}
func withoutAssociation(signature string) string {
	var v map[string]any
	if decodeFixture([]byte(signature), &v) != nil {
		return signature
	}
	delete(v, "RESTType")
	delete(v, "RESTUsable")
	b, _ := json.Marshal(v)
	return string(b)
}

func (f *fixtureRun) awaitDetached(after time.Time) ([]probeapi.Unit, error) {
	return f.pollMachines(after, func(rows map[string]probeapi.Unit, meta probeapi.SweepInfo) (bool, error) {
		done := true
		for _, r := range f.ledger.Records {
			if r.Owned && strings.HasPrefix(r.Kind, "association:") {
				m := rows[r.ID].RestMachine
				if m == nil || m.ControllerID != text(r.Spec, "hostId") {
					return false, instanceIssue("owned host disappeared during association release")
				}
				done = done && m.InstanceType == nil
			}
		}
		if done {
			return true, f.in.w.Record(f.in.ctx, "fixtureAssociationsReleased", meta)
		}
		return false, nil
	})
}
func (f *fixtureRun) pollMachines(after time.Time, settled func(map[string]probeapi.Unit, probeapi.SweepInfo) (bool, error)) ([]probeapi.Unit, error) {
	ctx, cancel := context.WithDeadline(f.in.ctx, f.end)
	defer cancel()
	for {
		rows, meta, err := probe.SnapshotUnits(ctx, f.p, f.end, probeapi.Filter{Type: probeapi.UnitTypeHost, Source: new(probeapi.SourceRest)})
		if err != nil {
			return nil, instanceObservationError(err)
		}
		if err = sourceMeta(meta, probeapi.FamilyRestMachines, f.epoch, float64(f.in.t.s.Observation.IntervalSeconds)); err != nil {
			return nil, err
		}
		if meta.SweepStart.After(after) {
			byID := map[string]probeapi.Unit{}
			for _, u := range rows {
				byID[u.ID] = u
			}
			done, e := settled(byID, meta)
			if done || e != nil {
				return rows, e
			}
		}
		if !time.Now().Before(f.end) {
			return nil, instanceIssue("host associations did not settle")
		}
		if err = waitInstance(ctx, max(10, f.in.t.s.Observation.IntervalSeconds)); err != nil {
			return nil, err
		}
	}
}

// An allocatability-only change outside the cohort is ordinary live-site
// movement. Keep the original invariant baseline and journal each observation.
func (f *fixtureRun) background(id, before, after string, next *instanceSnapshot) error {
	a, err := object([]byte(before))
	if err != nil {
		return err
	}
	b, err := object([]byte(after))
	if err != nil {
		return err
	}
	delete(a, "Allocatable")
	delete(b, "Allocatable")
	if !containsSpec(a, b) || !containsSpec(b, a) {
		return instanceIssue("outside-cohort fixture identity or binding changed")
	}
	if f.observed == nil {
		f.observed = map[string]transition{}
	}
	event := transition{Host: id, Class: "fixture-background", Burst: -1, Before: before, After: after, BeforeSource: new(f.in.plan.Sources[probeapi.FamilyHost]), Source: next.meta[probeapi.FamilyHost], Attribution: "undetermined"}
	if previous, ok := f.observed[id]; ok {
		event.Before = previous.After
		event.BeforeSource = new(previous.Source)
	}
	if event.Before == event.After {
		return nil
	}
	if err = f.in.w.Record(f.in.ctx, "fixtureBackground", event); err != nil {
		return err
	}
	f.observed[id] = event
	if len(f.in.d.BackgroundChanges) < 8 {
		f.in.d.BackgroundChanges = append(f.in.d.BackgroundChanges, event)
	}
	return nil
}

func (f *fixtureRun) checkFrozen(next *instanceSnapshot) error {
	if f.cleanupHosts != nil {
		for _, r := range f.ledger.Records {
			if !r.Owned || !strings.HasPrefix(r.Kind, "association:") {
				continue
			}
			host := text(r.Spec, "hostId")
			u, rest := next.rows[probeapi.FamilyHost][host], next.rest[host]
			m := rest.RestMachine
			if rest.ID != r.ID || !readyHost(u) || !restHealthy(rest) || m.InstanceID != nil || m.TenantID != nil || m.InstanceType != nil && *m.InstanceType != text(r.Spec, "instanceTypeId") || m.Labels[instanceLabel] != "" {
				return instanceIssue("owned cleanup association identity or eligibility changed")
			}
			if len(u.Host.DpuIDs) == 0 {
				return instanceIssue("owned cleanup host has no DPU evidence")
			}
			for _, id := range u.Host.DpuIDs {
				d := next.rows[probeapi.FamilyDpu][id]
				if d.Stage != probeapi.StageReady || d.Dpu == nil || d.Dpu.HostID == nil || *d.Dpu.HostID != host {
					return instanceIssue("owned cleanup DPU is not Ready or its binding changed")
				}
			}
		}
	}
	for _, h := range f.in.plan.Cohort {
		if err := f.in.hostIdentity(h, next); err != nil {
			return err
		}
		u, r := next.rows[probeapi.FamilyHost][h.ID], next.rest[h.ID]
		m := r.RestMachine
		if !readyHost(u) || !restHealthy(r) || m.InstanceID != nil || m.TenantID != nil || m.InstanceType != nil && *m.InstanceType != f.kind || m.Labels[instanceLabel] != "" && m.Labels[instanceLabel] != f.in.selectorValue(h.Burst) || !capabilitiesMatch(m.MachineCapabilities, f.in.t.s.Fixture.MachineCapabilities) {
			return instanceIssue("frozen fixture host lost eligibility")
		}
		for _, id := range h.DPUs {
			if next.rows[probeapi.FamilyDpu][id].Stage != probeapi.StageReady {
				return instanceIssue("frozen fixture DPU is not Ready")
			}
		}
	}
	return nil
}

func syncFixtureDir(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	return errors.Join(dir.Sync(), dir.Close())
}
