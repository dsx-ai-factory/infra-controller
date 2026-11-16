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
	"net/http"
	"slices"
	"sync"
	"sync/atomic"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

const instanceLabel = "scale-test-instance"

var errCohort = instanceIssue("a cohort member failed or an owned instance escaped its burst")

// InstanceTest shares readiness, dispatch control and stored-sample timing for
// create and owned delete. No NICo or REST state read is available here.
type InstanceTest struct {
	cfg   *config.Config
	s     InstanceSettings
	deps  readiness.Deps
	p     probe.Probe
	rest  *restWrites
	check func(context.Context, readiness.Deps, time.Time) readiness.Report
}

// ProductionInstances wires the existing probe/store fixture and a writes-only adapter.
func ProductionInstances(cfg *config.Config, s InstanceSettings, d readiness.Deps) (*InstanceTest, error) {
	p, ok := d.Probe.(probe.Probe)
	if !ok {
		return nil, instanceIssue("probe cannot page instance evidence")
	}
	if s.Fixture.Name != "" {
		rp, e := probe.NewHTTP(s.Fixture.ProbeURL, 40*time.Second)
		if e != nil {
			return nil, e
		}
		s.sidecar = rp.(fixtureProbe)
		s.preparing = true
	}
	rest, err := newREST(s)
	if err != nil {
		return nil, err
	}
	return &InstanceTest{cfg, s, d, p, rest, readiness.Populated}, nil
}
func (t *InstanceTest) journalID() string       { return cmp.Or(t.s.journalRunID, t.cfg.RunID) }
func (t *InstanceTest) Name() string            { return t.cfg.Command }
func (*InstanceTest) LockKind() lock.HolderKind { return lock.HolderTest }
func (*InstanceTest) Writes() bool              { return true }

type instanceStore interface {
	Summarizer
	PoolHealthy(context.Context, time.Time, map[string]string) ([]string, error)
	ProbeDemand(context.Context, time.Time, time.Duration, ...float64) error
}
type instanceBaseline struct {
	cp, mat                map[string]podState
	pools                  []string
	nativeEpoch, restEpoch float64
}
type instanceHealth struct {
	at  time.Time
	err error
}

type instanceRun struct {
	t            *InstanceTest
	rc           run.RunCtx
	w            *writer.Writer
	ctx          context.Context
	end          time.Time
	plan         instancePlan
	latest       *instanceSnapshot
	previous     map[string]string
	classes      map[string]string
	d            instanceDetail
	evidence     *instanceEvidence
	store        instanceStore
	cp, mat      map[string]podState
	pools        []string
	safetyMu     sync.Mutex
	health       atomic.Pointer[instanceHealth]
	writeRefresh time.Time
	healthStop   chan struct{}
	epoch        float64
	names        map[string]int
	created      map[string]int
	deleting     bool
	active       int
	sequence     int
	observation  int
	script       string
	scopes       [][]int
	selectedPods []string
	positions    map[string]int
	cohortSaved  bool
	stages       []result.StageResult

	fixture           *fixtureRun
	fixturePhase      bool
	fixtureRESTClosed bool
	stageScopes       [][]int
	remaining         []CohortHost
}

func (t *InstanceTest) Run(rc run.RunCtx, w *writer.Writer) (out result.Outcome, err error) {
	if t.Name() != "instance-lifecycle" {
		return t.run(rc, w)
	}
	child := *t
	cfg := *t.cfg
	child.cfg = &cfg
	child.s.journalRunID = t.cfg.RunID
	phases := map[string]json.RawMessage{}
	var stages []result.StageResult
	for _, phase := range []string{"create", "delete"} {
		cfg.Command = "instance-" + phase
		cfg.RunID = phase + "-" + digest([]byte(t.cfg.RunID))[:32]
		child.s.preparing = child.s.Fixture.Name != ""
		out, err = child.run(rc, w)
		phases[phase] = out.Detail
		stages = append(stages, out.Stages...)
		if err != nil || out.Verdict != result.Passed {
			break
		}
		var detail instanceDetail
		if json.Unmarshal(out.Detail, &detail) != nil || detail.Manifest == "" {
			err = errors.New("lifecycle phase has no owned manifest")
			out.Verdict = result.Unresolved
			break
		}
		child.s.Instance.OwnedManifest = detail.Manifest
	}
	out.Detail, _ = json.Marshal(phases)
	out.Stages = stages
	return out, err
}
func (t *InstanceTest) run(rc run.RunCtx, w *writer.Writer) (out result.Outcome, err error) {
	end := rc.Clock.Instant().Add(rc.Deadline.Sub(rc.Clock.Now()))
	ctx, cancel := context.WithDeadline(rc.Ctx, end)
	defer cancel()
	in := &instanceRun{t: t, rc: rc, w: w, ctx: ctx, end: end, deleting: t.Name() == "instance-delete", active: -1, healthStop: make(chan struct{}), names: map[string]int{}, created: map[string]int{}}
	in.d.Evidence = map[string]any{}
	defer func() {
		if len(in.plan.Cohort) > 0 && !in.cohortSaved && in.evidence != nil {
			err = errors.Join(err, in.saveCohort())
		}
		if in.evidence != nil {
			finishErr := in.evidence.finish()
			err = errors.Join(err, finishErr)
			in.d.Evidence["transitions"] = map[string]any{"path": in.evidence.path, "sha256": in.evidence.hash, "entries": in.evidence.count, "bytes": in.evidence.bytes, "inlineOverflow": in.evidence.count > len(in.d.CohortTimeline)+len(in.d.BackgroundChanges)}
		}
		err = errors.Join(err, rc.Ctx.Err())
		if !time.Now().Before(end) {
			err = errors.Join(err, context.DeadlineExceeded)
		}
		if errors.Is(err, errCohort) {
			out.Verdict = result.Failed
		} else if err != nil && out.Verdict == result.Passed {
			out.Verdict = result.Unresolved
		}
		for b := range in.stages {
			scope := in.stageScopes[b]
			failed := uint64(0)
			for _, i := range scope {
				if in.plan.Cohort[i].FirstFailure != "" {
					failed++
				}
			}
			if failed > 0 {
				in.stages[b].Status = result.StageFailed
				in.stages[b].Units = result.UnitCountsOf(uint64(len(scope)), 0, failed)
			}
		}
		if in.fixture != nil {
			report := in.fixture.report
			if phase, ok := report["measurement"].(map[string]any); ok && phase["verdict"] == "running" {
				phase["verdict"] = out.Verdict
			}
		}
		out = in.d.outcome(out.Verdict, err)
		out.Stages = in.stages
		if raw, e := json.Marshal(out.Stages); e == nil && len(raw) > int(t.cfg.ResultMaxBytes)/4 {
			done, failed := uint64(0), uint64(0)
			for _, s := range in.stages {
				done += s.Units.Done
				failed += s.Units.Failed
			}
			out.Stages = []result.StageResult{{ID: result.StageID(t.Name()), Status: result.StageUnresolved, Units: result.UnitCountsOf(uint64(len(in.plan.Cohort)), done, failed)}}
			if out.Verdict == result.Passed {
				out.Stages[0].Status = result.StageDone
			}
			in.d.Evidence["stagesSpilled"] = true
			out.Detail = in.d.outcome(out.Verdict, err).Detail
		}
		err = instanceExecutionError(err)
	}()
	out = result.OutcomeOf(result.NotReady)
	window := time.Duration(t.cfg.Timeouts.ReadinessSeconds) * time.Second
	d := t.deps
	d.Now = rc.Clock.Now
	rep := t.check(ctx, d, minTime(end, rc.Clock.Instant().Add(window)))
	in.d.Checks = rep.Checks
	if !rep.Ready() {
		return out, instanceIssue("populated fixture readiness failed")
	}
	cp := capture(ctx, d.Pods, t.cfg.ControlPlane, true)
	if cp.Fail != "" {
		return out, instanceIssue("control-plane baseline failed")
	}
	in.cp = cp.Pods
	matSelector := []config.PodSelector{{Namespace: t.cfg.Namespace, LabelSelector: t.s.Mat.Selector}}
	mat := capture(ctx, d.Pods, matSelector)
	if mat.Fail != "" {
		return out, instanceIssue("MAT baseline failed")
	}
	in.mat = mat.Pods
	if len(in.cp) == 0 || len(in.mat) == 0 {
		return out, instanceIssue("pod baselines are empty")
	}
	if d.Store == nil {
		return out, instanceIssue("metrics store is not configured")
	}
	sp, e := d.Store(ctx)
	if e != nil {
		return out, instanceObservationError(e)
	}
	in.store, _ = sp.Client.(instanceStore)
	if in.store == nil {
		return out, instanceIssue("store cannot check instance timing and API pool health")
	}
	in.fixturePhase = t.s.Fixture.Name != ""
	if b := t.s.lifecycleBase; b != nil {
		in.cp, in.mat, in.pools, in.epoch = b.cp, b.mat, b.pools, b.nativeEpoch
		t.s.restEpoch = b.restEpoch
	}
	if err = in.safety(); err != nil {
		return out, err
	}
	if t.s.Fixture.Name != "" {
		in.fixture, err = in.beginFixture(rep.Inventory)
		if err == nil {
			if in.deleting {
				err = in.fixture.reuse()
			} else {
				err = in.fixture.setup()
			}
		}
		if in.fixture != nil {
			err = in.fixture.finishSetup(err)
		}
		if err != nil {
			return out, err
		}
		if rc.DryRun && !in.deleting {
			return result.OutcomeOf(result.Go), nil
		}
		t.s.tenantCensus = in.deleting && len(in.plan.Cohort) == 0 && t.s.Fixture.Teardown
		t.s.preparing, in.fixturePhase = t.s.tenantCensus, t.s.tenantCensus
		if t.s.tenantCensus {
			in.fixture.report["measurement"] = map[string]string{"verdict": "skipped", "reason": "cleanup-only"}
		}
	}
	if err = in.calibrate(minTime(end, rc.Clock.Instant().Add(window))); err != nil {
		return out, err
	}
	if t.s.tenantCensus && (len(in.latest.rows[probeapi.FamilyInstance]) != 0 || len(in.latest.rows[probeapi.FamilyRestInstances]) != 0) {
		return out, instanceIssue("cleanup requires a current empty native and REST tenant")
	}
	if !in.deleting && in.latest.config.Rest.TenantAllocation.UnusedUsable < int64(t.s.Instance.N) {
		return out, instanceIssue("tenant usable allocation is below requested N")
	}
	if in.deleting {
		in.plan, err = readOwned(t.cfg.Journal.Dir, t.s.Instance.OwnedManifest)
		if err == nil {
			err = in.owned()
			if err == nil {
				err = in.selectDelete()
			}
		}
	} else if in.fixture == nil {
		in.plan, err = planInstances(in.latest, rep.Inventory, in.mat, t.s, t.cfg.RunID, t.cfg.Namespace)
	} else {
		in.plan.Tenant, in.plan.Type = t.s.Instance.TenantID, t.s.Instance.InstanceTypeID
		in.plan.Sources = in.latest.meta
		if err = in.fixture.checkFrozen(in.latest); err != nil {
			return out, err
		}
		for _, h := range in.plan.Cohort {
			if in.latest.unstable[h.ID] || !readyHost(in.latest.rows[probeapi.FamilyHost][h.ID]) || !restFree(in.latest.rest[h.ID], in.plan.Type) {
				return out, instanceIssue("frozen fixture cohort lost eligibility")
			}
		}
		for id, before := range in.plan.Baseline {
			_, selected := in.fixture.ledger.Cohort[id]
			if !selected {
				if err = in.fixture.background(id, before, hostSignature(in.latest, id), in.latest); err != nil {
					return out, err
				}
			}
		}
	}
	if err != nil {
		return out, err
	}
	if in.deleting {
		in.plan.SourceOwned = map[string]string{"path": t.s.Instance.OwnedManifest, "sha256": in.plan.sourceHash}
		in.plan.Sources = in.latest.meta
		in.plan.Baseline = map[string]string{}
		for id := range in.latest.rows[probeapi.FamilyHost] {
			in.plan.Baseline[id] = hostSignature(in.latest, id)
		}
		for i := range in.plan.Cohort {
			in.plan.Cohort[i].Initial = hostSignature(in.latest, in.plan.Cohort[i].ID)
			in.plan.Cohort[i].Final = ""
			in.plan.Cohort[i].FirstFailure = ""
		}
	}
	in.plan.CommandRunID = t.cfg.RunID
	in.positions = map[string]int{}
	for i, h := range in.plan.Cohort {
		in.positions[h.ID] = i
		key := t.cfg.Namespace + "/" + h.Pod
		if !slices.Contains(in.selectedPods, key) {
			in.selectedPods = append(in.selectedPods, key)
		}
		for len(in.scopes) <= h.Burst {
			in.scopes = append(in.scopes, nil)
		}
		in.scopes[h.Burst] = append(in.scopes[h.Burst], i)
	}
	if !in.deleting {
		in.plan.OriginalCount = len(in.plan.Cohort)
		if err = in.planNames(); err != nil {
			return out, err
		}
		in.script, err = readScript(t.s.Instance.IPXEFile)
		if err != nil {
			return out, err
		}
		in.plan.ScriptSHA = digest([]byte(in.script))
		in.plan.OwnerJournalRunID = t.journalID()
	}
	in.previous = map[string]string{}
	in.classes = map[string]string{}
	for id := range in.latest.rows[probeapi.FamilyHost] {
		in.previous[id] = hostSignature(in.latest, id)
		in.classes[id] = "untouched-other"
		if _, ok := in.plan.Excluded[id]; ok {
			in.classes[id] = "excluded-at-plan"
		}
	}
	for _, h := range in.plan.Cohort {
		in.classes[h.ID] = "in-cohort"
	}
	invName := t.cfg.RunID + ".inventory.json"
	invHash, e := writeOnce(t.cfg.Journal.Dir, invName, rep.Inventory)
	if e != nil {
		return out, e
	}
	in.plan.Inventory = map[string]string{"path": invName, "sha256": invHash}
	in.plan.Network = map[string]string{"vpc": t.s.Instance.VPCID, "subnet": t.s.Instance.SubnetID, "ipxeSha256": in.plan.ScriptSHA, "phoneHomeEnabled": "false"}
	in.plan.Health = map[string]any{"poolSelector": t.s.Instance.APIPoolSelector, "poolReplicas": t.s.Instance.APIPoolReplicas, "controlPlane": in.cp, "matPods": in.mat}
	planName := t.cfg.RunID + ".instance-plan.json"
	hash, e := writeOnce(t.cfg.Journal.Dir, planName, in.plan)
	if e != nil {
		return out, e
	}
	if err = w.Record(ctx, "instancePlan", map[string]any{"path": planName, "sha256": hash}); err != nil {
		return out, err
	}
	in.d.Cohort = map[string]any{"path": planName, "sha256": hash, "count": len(in.plan.Cohort)}
	in.evidence, err = openEvidence(t.cfg.Journal.Dir, t.cfg.RunID)
	if err != nil {
		return out, err
	}
	in.epoch = in.latest.config.Metrics.ProcessStartSeconds
	in.d.Calibration = in.latest.meta
	healthCtx, stopHealth := context.WithCancel(ctx)
	healthDone := make(chan struct{})
	healthError := make(chan error, 1)
	go func() {
		defer close(healthDone)
		tick := time.NewTicker(15 * time.Second)
		defer tick.Stop()
		for {
			select {
			case <-healthCtx.Done():
				return
			case <-tick.C:
				if e := in.safety(); e != nil {
					healthError <- e
					close(in.healthStop)
					return
				}
			}
		}
	}()
	defer func() {
		stopHealth()
		<-healthDone
		select {
		case e := <-healthError:
			in.d.Evidence["safetyStop"] = map[string]any{"reason": e.Error(), "evidence": e}
			err = errors.Join(err, e)
		default:
		}
	}()
	if rc.DryRun {
		for burst, scope := range in.scopes {
			in.active = burst
			if in.deleting {
				for _, i := range scope {
					h := in.plan.Cohort[i]
					if t.s.Instance.DeleteCount == 0 && in.released(h) {
						continue
					}
					if _, err = in.send("delete", http.MethodDelete, "instance/"+h.InstanceID, nil, 0, h.ID); err != nil {
						return out, err
					}
				}
			} else {
				for _, i := range scope {
					h := in.plan.Cohort[i]
					labels := maps.Clone(in.latest.rest[h.ID].RestMachine.Labels)
					if labels == nil {
						labels = map[string]string{}
					}
					labels[instanceLabel] = in.selectorValue(burst)
					if _, err = in.send("labels", http.MethodPatch, "machine/"+h.RestID, map[string]any{"labels": labels}, 0, h.ID); err != nil {
						return out, err
					}
				}
				for _, request := range in.plan.Requests {
					if request.Burst == burst {
						if _, err = in.send("create", http.MethodPost, "instance/batch", in.createBody(request), request.Count, scopeIDs(scope, in.plan.Cohort)...); err != nil {
							return out, err
						}
					}
				}
			}
		}
		return result.OutcomeOf(result.Go), nil
	}
	out = result.OutcomeOf(result.Unresolved)
	if in.fixture != nil && !t.s.tenantCensus {
		in.fixture.report["measurement"] = map[string]any{"verdict": "running", "startedAt": time.Now()}
	}
	for burst, scope := range in.scopes {
		if len(scope) == 0 {
			continue
		}
		in.active = burst
		if err = in.burst(burst); err != nil {
			return out, err
		}
	}
	if err = in.await(time.Now(), instanceSettlementFamilies, func() bool { return in.settled(in.active) && (!in.deleting || in.labelsCleared(in.active)) }); err != nil {
		return out, err
	}
	if err = in.saveCohort(); err != nil {
		return out, err
	}
	owned := in.plan
	if in.deleting {
		owned.Cohort = in.remaining
	}
	in.d.Manifest = t.cfg.RunID + ".owned.json"
	if _, e = writeOnce(t.cfg.Journal.Dir, in.d.Manifest, owned); e != nil {
		return out, e
	}
	if in.fixture != nil {
		if !t.s.tenantCensus && len(in.stages) > 0 {
			in.fixture.report["measurement"] = map[string]any{"verdict": "passed", "finishedAt": time.Now()}
		} else if !t.s.tenantCensus {
			in.fixture.report["measurement"] = map[string]string{"verdict": "skipped", "reason": "already released"}
		}
		if in.deleting && len(in.remaining) == 0 && t.s.Fixture.Teardown {
			in.safetyMu.Lock()
			in.fixturePhase = true
			in.safetyMu.Unlock()
			if err = in.fixture.teardown(); err != nil {
				return out, err
			}
		}
		if err = in.fixture.commitOwner(); err != nil {
			return out, err
		}
	}
	if err = in.evidence.finish(); err != nil {
		return out, err
	}
	stopHealth()
	<-healthDone
	select {
	case e := <-healthError:
		in.d.Evidence["safetyStop"] = map[string]any{"reason": e.Error(), "evidence": e}
		return out, e
	default:
	}
	// A lifecycle retains its cleanup obligation until delete has settled.
	if in.deleting || t.s.journalRunID == "" {
		if err = w.Pending(ctx, false); err != nil {
			return out, err
		}
	}
	if !in.deleting && t.s.journalRunID != "" {
		t.s.lifecycleBase = &instanceBaseline{in.cp, in.mat, in.pools, in.epoch, in.latest.restEpoch}
	}
	return result.OutcomeOf(result.Passed), nil
}

func (in *instanceRun) safety(until ...time.Time) (err error) {
	in.safetyMu.Lock()
	defer in.safetyMu.Unlock()
	var health instanceHealth
	if previous := in.health.Load(); previous != nil {
		if previous.err != nil || time.Since(previous.at) < 15*time.Second {
			return previous.err
		}
		health.at = previous.at
	}
	defer func() {
		health.err = err
		if err == nil {
			health.at = time.Now()
		}
		in.health.Store(&health)
	}()
	deadline := minTime(in.end, time.Now().Add(15*time.Second))
	if len(until) > 0 {
		deadline = minTime(deadline, until[0])
	}
	ctx, cancel := context.WithDeadline(in.ctx, deadline)
	defer cancel()
	cfg, e := in.t.p.Config(ctx, probe.ConfigFilter{InstanceType: &in.t.s.Instance.InstanceTypeID})
	if e != nil {
		return instanceObservationError(e)
	}
	if cfg.Metrics == nil || in.epoch != 0 && cfg.Metrics.ProcessStartSeconds != in.epoch {
		return instanceIssue("native probe process changed")
	}
	if in.t.s.sidecar == nil && (cfg.Rest == nil || cfg.RestSite == nil || cfg.Rest.LastAttemptError != nil || cfg.RestSite.LastAttemptError != nil || cfg.RestSite.Result != probeapi.SiteMatchMatch) {
		return instanceIssue("probe REST configuration failed")
	}
	if in.t.s.sidecar != nil && !in.fixturePhase && !in.fixtureRESTClosed {
		rest, e := in.t.s.sidecar.Config(ctx, probe.ConfigFilter{InstanceType: &in.t.s.Instance.InstanceTypeID})
		if e != nil {
			return instanceObservationError(e)
		}
		if e = instanceAllocation(rest, in.t.s); e != nil {
			return e
		}
	}
	for _, spec := range instanceFamilies {
		reader, epoch := in.t.p, cfg.Metrics.ProcessStartSeconds
		if spec.filter.Source != nil {
			if in.fixtureRESTClosed || in.fixturePhase && (spec.family == probeapi.FamilyRestInstances || in.t.s.restEpoch == 0) {
				continue
			}
			if in.t.s.sidecar != nil {
				reader, epoch = in.t.s.sidecar, in.t.s.restEpoch
			}
		}
		page, e := reader.Units(ctx, spec.filter, nil, probe.Page{Limit: new(uint32(1))})
		if e != nil {
			return instanceObservationError(e)
		}
		if in.fixturePhase && spec.filter.Source != nil && epoch == 0 {
			continue
		}
		if e = sourceMeta(page.SweepInfo, spec.family, epoch, float64(in.t.s.Observation.IntervalSeconds)); e != nil {
			return e
		}
	}
	pools, err := in.store.PoolHealthy(ctx, in.end, in.t.s.Instance.APIPoolSelector)
	if err != nil {
		return instanceObservationError(err)
	}
	if uint64(len(pools)) != in.t.s.Instance.APIPoolReplicas {
		return instanceIssue("API pool replica coverage is incomplete")
	}
	if in.pools != nil && !slices.Equal(in.pools, pools) {
		return instanceIssue("API pool replica identities changed")
	}
	in.pools = pools
	for _, set := range []struct {
		selectors []config.PodSelector
		baseline  map[string]podState
		mat       bool
	}{
		{in.t.cfg.ControlPlane, in.cp, false}, {[]config.PodSelector{{Namespace: in.t.cfg.Namespace, LabelSelector: in.t.s.Mat.Selector}}, in.mat, true},
	} {
		now := capture(ctx, in.t.deps.Pods, set.selectors, !set.mat)
		if set.mat {
			for _, key := range in.selectedPods {
				if now.Unready[key] {
					return instanceIssue("selected MAT pod is not Ready")
				}
			}
		}
		if now.Fail != "" || !maps.Equal(now.Pods, set.baseline) {
			return &instanceSafetyFailure{Cause: "control-plane or MAT pod identity, readiness or restart count changed", Before: set.baseline, After: now.Pods}
		}
	}
	return nil
}

func (in *instanceRun) calibrate(until time.Time) (err error) {
	ctx, cancel := context.WithDeadline(in.ctx, until)
	defer cancel()
	var first, last *instanceSnapshot
	changes := []transition{}
	used := 2
	defer func() {
		if len(changes) > 0 {
			err = errors.Join(err, in.saveCalibrationChanges(changes))
		}
	}()
	for {
		next, err := readInstanceSnapshot(ctx, in.t.p, until, in.t.s, last)
		if err != nil {
			var pending *probe.Error
			if in.fixture != nil && last == nil && errors.As(err, &pending) && pending.Kind == probe.NoCompleteSweep {
				if err = waitInstance(ctx, in.t.cfg.Probe.PollSeconds); err == nil {
					continue
				}
			}
			return err
		}
		if last != nil {
			if err = in.calibrationChanges(last, next, &changes, &used); err != nil {
				return err
			}
		}
		last = next
		if first != nil {
			advanced := true
			for _, spec := range instanceFamilies {
				if in.t.s.preparing && !in.t.s.tenantCensus && spec.family == probeapi.FamilyRestInstances {
					continue
				}
				advanced = advanced && next.meta[spec.family].SweepStart.After(first.meta[spec.family].ReadAt.Time)
			}
			if advanced {
				if len(next.rows[probeapi.FamilyHost]) == 0 {
					return instanceIssue("populated calibration has no hosts")
				}
				extra := 0.0
				if in.t.s.sidecar != nil {
					elapsed := next.sidecarRead.Sub(first.sidecarRead).Seconds()
					if next.restEpoch != first.restEpoch || next.sidecarCalls < first.sidecarCalls || !(elapsed > 0) {
						return instanceIssue("REST sidecar call accounting changed")
					}
					extra = float64(next.sidecarCalls-first.sidecarCalls+1) / elapsed
				}
				if err = in.store.ProbeDemand(ctx, until, time.Duration(in.t.s.Observation.IntervalSeconds)*time.Second, extra); err != nil {
					return instanceObservationError(err)
				}
				in.latest = next
				if in.t.s.preparing {
					return nil
				}
				return in.agreement(next)
			}
		} else {
			first = next
		}
		if err = waitInstance(ctx, in.t.cfg.Probe.PollSeconds); err != nil {
			return err
		}
	}
}

const errSafetyStop = instanceIssue("safety stop")

func waitInstance(ctx context.Context, seconds uint64, stopped ...<-chan struct{}) error {
	var stop <-chan struct{}
	if len(stopped) > 0 {
		stop = stopped[0]
	}
	timer := time.NewTimer(time.Duration(max(seconds, 1)) * time.Second)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return instanceObservationError(ctx.Err())
	case <-stop:
		return errSafetyStop
	case <-timer.C:
		return nil
	}
}

// Calibration precedes T0. Retain background evidence and exclude changing
// candidates instead of requiring an idle site. Measurement keeps its existing
// background-counter invalidation rules.
func (in *instanceRun) calibrationChanges(first, next *instanceSnapshot, changes *[]transition, used *int) error {
	next.unstable = maps.Clone(first.unstable)
	if next.unstable == nil {
		next.unstable = map[string]bool{}
	}
	ids := map[string]bool{}
	for id := range first.rows[probeapi.FamilyHost] {
		ids[id] = true
	}
	for id := range next.rows[probeapi.FamilyHost] {
		ids[id] = true
	}
	for _, id := range slices.Sorted(maps.Keys(ids)) {
		before, after := hostSignature(first, id), hostSignature(next, id)
		if before == after {
			continue
		}
		next.unstable[id] = true
		event := transition{Observation: 0, Host: id, Class: "calibration-background", Burst: -1, Before: before, After: after, Source: next.meta[probeapi.FamilyHost], BeforeSource: new(first.meta[probeapi.FamilyHost]), Attribution: "undetermined", Sources: next.meta, BeforeSources: first.meta}
		raw, err := json.Marshal(event)
		if err != nil {
			return err
		}
		*used += len(raw) + 1
		if *used > 128<<20 {
			return instanceIssue("calibration background evidence quota exhausted")
		}
		*changes = append(*changes, event)
		if len(in.d.BackgroundChanges) < 8 {
			in.d.BackgroundChanges = append(in.d.BackgroundChanges, event)
		}
	}
	return nil
}
func (in *instanceRun) saveCalibrationChanges(changes []transition) error {
	phase := "calibration"
	if in.t.s.preparing && !in.t.s.tenantCensus {
		phase = "fixture-calibration"
	}
	file := in.t.cfg.RunID + "." + phase + "-background.json"
	hash, err := writeOnce(in.t.cfg.Journal.Dir, file, changes)
	if err != nil {
		return err
	}
	ref := map[string]any{"path": file, "sha256": hash, "entries": len(changes)}
	in.d.Evidence[phase+"Background"] = ref
	return in.w.Record(in.ctx, "calibrationBackground", ref)
}

func (in *instanceRun) planNames() error {
	for burst, scope := range in.scopes {
		sizes, err := batches(uint64(len(scope)), 18)
		if err != nil {
			return err
		}
		for call, size := range sizes {
			prefix := fmt.Sprintf("st-%s-b%d-c%d", in.t.cfg.RunID, burst, call)
			in.plan.Requests = append(in.plan.Requests, instanceRequest{Burst: burst, Prefix: prefix, Count: size})
			for slot := 1; slot <= size; slot++ {
				in.names[fmt.Sprintf("%s-%d", prefix, slot)] = burst
			}
		}
	}
	return nil
}

func (in *instanceRun) send(kind, method, path string, body any, count int, hosts ...string) (writer.Outcome, error) {
	var out writer.Outcome
	if err := in.safety(); err != nil {
		return out, err
	}
	check := func() error {
		if err := validToken(in.t.rest.until); err != nil {
			return err
		}
		if err := in.dispatchEvidence(); err != nil {
			return err
		}
		return in.writePreconditions(kind, body, count, hosts)
	}
	raw, err := json.Marshal(body)
	if err != nil {
		return out, err
	}
	if body == nil {
		raw = nil
	}
	in.sequence++
	call := writer.Call{WriteCall: journal.WriteCall{Scope: in.t.Name(), Kind: kind, Key: fmt.Sprintf("%s/%s/%s/%d", in.t.cfg.RunID, kind, path, in.sequence), Hosts: hosts, RequestDigest: digest(append([]byte(method+" "+path+"\n"), raw...))}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: time.Minute}, Cutoff: in.end}
	call.BeforeDispatch = check
	if kind == "labels" {
		call.Idempotent, call.Retry.MaxAttempts = true, 2
		call.RetryCutoff = in.end.Add(-time.Minute - time.Duration(in.t.s.Observation.LabelReadbackSeconds)*time.Second)
		retried := false
		call.ReconcileUnclear = func(op uint64) (bool, error) {
			host := slices.IndexFunc(in.plan.Cohort, func(h CohortHost) bool { return h.ID == hosts[0] })
			labels := body.(map[string]any)["labels"].(map[string]string)
			_, adding := labels[instanceLabel]
			err := in.settleLabel(in.plan.Cohort[host], op, !adding, time.Now())
			if errors.Is(err, errLabelAbsent) && !retried {
				if err = errors.Join(in.t.rest.authorize(in.ctx, true), in.safety()); err != nil {
					return false, err
				}
				retried = true
				return false, nil
			}
			return err == nil, err
		}
	}
	if !in.rc.DryRun {
		if err = in.t.rest.pace.Wait(in.ctx); err != nil {
			return out, err
		}
	}
	if err = in.refreshForWrite(); err != nil {
		return out, err
	}
	if !in.rc.DryRun {
		if err = in.t.rest.authorize(in.ctx, kind == "labels"); err != nil {
			return out, err
		}
	}
	if err = in.safety(); err != nil {
		return out, err
	}
	if err = check(); err != nil {
		return out, err
	}
	if in.rc.DryRun {
		return writer.Plan(in.ctx, in.w, call)
	}
	out, err = in.write(call, func(ctx context.Context) writer.Attempt { return in.t.rest.attempt(ctx, method, path, raw, count) })
	if kind == "labels" && (err != nil || out.Kind == writer.Unclear) {
		if err == nil {
			err = instanceIssue("label remains unclear after retry")
		}
		return out, err
	}
	if err == nil && out.Kind != writer.Accepted {
		err = instanceIssue("REST mutation was rejected or has an unclear reply; no replay is permitted")
	}
	var op *writer.OpError
	if out.Kind == writer.Unclear || errors.As(err, &op) && op.Dispatched {
		// Fresh read-back records any visible result, but never settles an
		// unclear create or authorizes a resend, even after an empty scan.
		err = errors.Join(err, in.await(time.Now(), []probeapi.Family{probeapi.FamilyHost, probeapi.FamilyInstance, probeapi.FamilyRestMachines, probeapi.FamilyRestInstances}, func() bool { return true }))
	}
	return out, err
}

func (in *instanceRun) dispatchEvidence() error {
	if time.Since(in.latest.oldestSource(in.fixtureRESTClosed)).Seconds() > 2*float64(in.t.s.Observation.IntervalSeconds) {
		return instanceIssue("source evidence expired before dispatch")
	}
	health := in.health.Load()
	if health == nil || health.err != nil || time.Since(health.at) >= 15*time.Second {
		return errSafetyStop
	}
	select {
	case <-in.healthStop:
		return errSafetyStop
	default:
		return nil
	}
}
func (in *instanceRun) write(call writer.Call, attempt func(context.Context) writer.Attempt) (out writer.Outcome, err error) {
	first := !in.w.Unsettled()
	if first {
		if err = in.w.Pending(in.ctx, true); err != nil {
			return out, err
		}
	}
	out, err = writer.Send(in.ctx, in.w, call, attempt)
	var notSent *writer.OpError
	if first && (out.Kind == writer.Failed || errors.As(err, &notSent) && !notSent.Dispatched) {
		err = errors.Join(err, in.w.Pending(in.ctx, false))
	}
	return out, err
}

func (in *instanceRun) refresh(until ...time.Time) error {
	end := in.end
	if len(until) > 0 {
		end = minTime(end, until[0])
	}
	ctx, cancel := context.WithDeadline(in.ctx, end)
	defer cancel()
	next, err := readInstanceSnapshot(ctx, in.t.p, end, in.t.s, in.latest)
	if err != nil {
		return err
	}
	if next.config.Metrics.ProcessStartSeconds != in.latest.config.Metrics.ProcessStartSeconds {
		return instanceIssue("probe process changed")
	}
	advanced := false
	for _, spec := range instanceFamilies {
		advanced = advanced || next.meta[spec.family].SnapshotID != in.latest.meta[spec.family].SnapshotID
	}
	if advanced {
		in.observation++
		if err = in.evidence.append(map[string]any{"observation": in.observation, "sources": next.meta}); err != nil {
			return err
		}
	}
	factsErr := in.transitions(next)
	in.latest = next
	return errors.Join(factsErr, in.agreement(next))
}

func (in *instanceRun) await(after time.Time, families []probeapi.Family, check func() bool) error {
	var settled *instanceSnapshot
	for {
		if err := in.refresh(); err != nil {
			return err
		}
		fresh := true
		for _, family := range families {
			fresh = fresh && in.latest.meta[family].SweepStart.After(after)
		}
		if fresh && check() {
			if settled != nil {
				advanced := true
				for _, family := range families {
					advanced = advanced && in.latest.meta[family].SweepStart.After(settled.meta[family].ReadAt.Time)
				}
				if advanced {
					return nil
				}
			}
			if settled == nil {
				settled = in.latest
			}
		} else {
			settled = nil
		}
		if err := waitInstance(in.ctx, in.t.cfg.Probe.PollSeconds, in.healthStop); err != nil {
			return err
		}
	}
}

func (in *instanceRun) burst(burst int) error {
	scope := in.scopes[burst]
	cleanup := scope
	if err := in.refresh(); err != nil {
		return err
	}
	if in.deleting {
		for _, h := range in.plan.Cohort {
			if (in.sequence == 0 || h.Burst == burst) && !in.deletable(h) && (in.t.s.Instance.DeleteCount != 0 || !in.released(h)) {
				return instanceIssue("selected owned instances are no longer Ready before delete")
			}
		}
		scope = slices.DeleteFunc(slices.Clone(scope), func(i int) bool { return in.t.s.Instance.DeleteCount == 0 && in.released(in.plan.Cohort[i]) })
		if len(scope) != len(cleanup) {
			if err := in.w.Record(in.ctx, "instanceAlreadyReleased", map[string]any{"hosts": scopeIDs(cleanup, in.plan.Cohort), "toDelete": scopeIDs(scope, in.plan.Cohort), "sources": in.latest.meta}); err != nil {
				return err
			}
		}
		if len(scope) == 0 {
			return in.clearLabels(burst, cleanup)
		}
	}
	if !in.deleting {
		last, err := in.labels(scope, false)
		if err != nil {
			return err
		}
		if err = in.await(last, []probeapi.Family{probeapi.FamilyRestMachines}, func() bool { return in.selector(scope) }); err != nil {
			return err
		}
	}
	filter, family := atStage(probeapi.UnitTypeInstance, probeapi.StageReady), probeapi.FamilyInstance
	if in.deleting {
		filter, family = hostGate(), probeapi.FamilyHost
	}
	base := in.base(filter, family)
	stageID := result.StageID(fmt.Sprintf("%s.b%d", in.t.Name(), burst))
	if err := in.w.Record(in.ctx, "burstBase", map[string]any{"burst": burst, "base": base, "n": len(scope), "hosts": scopeIDs(scope, in.plan.Cohort)}); err != nil {
		return err
	}
	stageIndex := len(in.stages)
	in.stages = append(in.stages, result.StageResult{ID: stageID, Status: result.StageUnresolved, Units: result.UnitCountsOf(uint64(len(scope)), 0, 0)})
	in.stageScopes = append(in.stageScopes, slices.Clone(scope))

	window := Window{}
	record := func(out writer.Outcome) {
		if window.T0.IsZero() {
			window.T0 = out.SentAt
		}
		window.End = out.RepliedAt
	}
	if in.deleting {
		for _, i := range scope {
			h := &in.plan.Cohort[i]
			out, err := in.send("delete", http.MethodDelete, "instance/"+h.InstanceID, nil, 0, h.ID)
			record(out)
			if err != nil {
				err = errors.Join(err, in.refresh())
				return err
			}
		}
	} else {
		for _, request := range in.plan.Requests {
			if request.Burst != burst {
				continue
			}
			body := in.createBody(request)
			out, err := in.send("create", http.MethodPost, "instance/batch", body, request.Count, scopeIDs(scope, in.plan.Cohort)...)
			record(out)
			for _, id := range out.IDs {
				if _, ok := in.created[id]; ok {
					return instanceIssue("created instance ID was reused")
				}
				in.created[id] = burst
			}
			if err != nil {
				err = errors.Join(err, in.refresh())
				return err
			}
		}
	}
	if err := in.await(window.End, instanceSettlementFamilies, func() bool { return in.settled(burst) }); err != nil {
		return err
	}
	until := time.Now()
	summary, err := in.store.Summarize(in.rc.Ctx, in.rc.EvidenceEnd, filter, probeapi.SummaryParams{Start: jsonx.Time{Time: window.T0}, End: jsonx.Time{Time: until}, N: uint64(len(scope)), Base: base, AddWindowSeconds: max(0, window.End.Sub(window.T0).Seconds())})
	if err != nil {
		return instanceObservationError(err)
	}
	event := map[string]any{"burst": burst, "base": base, "summary": summary, "t0": window.T0, "writeEnd": window.End}
	if err = in.evidence.append(event); err != nil {
		return err
	}
	if len(in.d.Bursts) < 8 {
		in.d.Bursts = append(in.d.Bursts, event)
	}
	status := result.StageDone
	if len(summary.Crossings) != len(probeapi.FixedQuantiles) || slices.ContainsFunc(summary.Crossings, func(c probeapi.Crossing) bool { return c.Seconds == nil }) {
		status = result.StageUnresolved
	}
	in.stages[stageIndex] = result.StageResult{ID: stageID, Status: status, Units: result.UnitCountsOf(uint64(len(scope)), uint64(len(scope)), 0)}
	if status != result.StageDone {
		return instanceIssue("stored samples do not prove the stage timing")
	}
	if in.deleting {
		return in.clearLabels(burst, cleanup)
	}
	return nil
}
func (in *instanceRun) clearLabels(burst int, scope []int) error {
	last, err := in.labels(scope, true)
	if err != nil {
		return err
	}
	return in.await(last, instanceSettlementFamilies, func() bool { return in.settled(burst) && in.labelsCleared(burst) })
}

var instanceSettlementFamilies = []probeapi.Family{probeapi.FamilyHost, probeapi.FamilyInstance, probeapi.FamilyRestMachines, probeapi.FamilyRestInstances}

func (in *instanceRun) base(f probeapi.Filter, family probeapi.Family) *probeapi.SummaryBase {
	n := uint64(0)
	for _, u := range in.latest.rows[family] {
		if f.Type == probeapi.UnitTypeHost && readyHost(u) || f.Type == probeapi.UnitTypeInstance && u.Stage == probeapi.StageReady {
			n++
		}
	}
	m := in.latest.meta[family]
	return &probeapi.SummaryBase{Count: n, ProcessStartSeconds: in.latest.config.Metrics.ProcessStartSeconds, SweepStart: m.SweepStart, ReadAt: m.ReadAt}
}

func (in *instanceRun) saveCohort() error {
	for i := range in.plan.Cohort {
		in.plan.Cohort[i].Final = hostSignature(in.latest, in.plan.Cohort[i].ID)
	}
	name := in.t.cfg.RunID + ".cohort.json"
	hash, err := writeOnce(in.t.cfg.Journal.Dir, name, in.plan.Cohort)
	if err == nil {
		in.cohortSaved = true
		in.d.Cohort = map[string]any{"path": name, "sha256": hash, "count": len(in.plan.Cohort)}
	}
	return err
}

func scopeIDs(scope []int, cohort []CohortHost) []string {
	ids := make([]string, len(scope))
	for j, i := range scope {
		ids[j] = cohort[i].ID
	}
	return ids
}

type instanceSafetyFailure struct {
	Cause  string              `json:"cause"`
	Before map[string]podState `json:"before"`
	After  map[string]podState `json:"after"`
}

func (e *instanceSafetyFailure) Error() string { return e.Cause }

func (in *instanceRun) createBody(request instanceRequest) map[string]any {
	body := map[string]any{"topologyOptimized": in.t.s.Instance.TopologyOptimized, "namePrefix": request.Prefix, "count": request.Count, "tenantId": in.plan.Tenant, "instanceTypeId": in.plan.Type, "vpcId": in.t.s.Instance.VPCID, "machineLabelSelector": map[string]string{instanceLabel: in.selectorValue(request.Burst)}, "ipxeScript": in.script, "phoneHomeEnabled": false, "interfaces": []any{map[string]string{"subnetId": in.t.s.Instance.SubnetID}}}
	if in.t.s.Instance.OperatingSystemID != "" {
		delete(body, "ipxeScript")
		body["operatingSystemId"] = in.t.s.Instance.OperatingSystemID
	}
	return body
}

// instanceIssue is a deliberate notReady, failed or unresolved reason. All
// untagged errors, including joined writer and evidence errors, remain fatal.
type instanceIssue string

func (e instanceIssue) Error() string { return string(e) }

type instanceObservationFailure struct{ error }

func (e instanceObservationFailure) Unwrap() error { return e.error }
func instanceObservationError(err error) error {
	if err == nil {
		return nil
	}
	return instanceObservationFailure{err}
}
func instanceExecutionError(err error) error {
	switch e := err.(type) {
	case instanceIssue, instanceObservationFailure, *instanceSafetyFailure:
		return nil
	case *writer.OpError:
		if instanceExecutionError(e.Err) == nil {
			return nil
		}
		return err
	case interface{ Unwrap() []error }:
		var hard []error
		for _, child := range e.Unwrap() {
			hard = append(hard, instanceExecutionError(child))
		}
		return errors.Join(hard...)
	default:
		return err
	}
}
