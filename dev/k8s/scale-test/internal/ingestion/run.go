// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"math"
	"slices"
	"strings"
	"time"

	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/fleetup"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// maxEntry is the longest text the result keeps from a read, a list entry or a pod change: cut keeps it plain, so it encodes at its length. maxLabels bounds
// the outcome labels: the fixed causes and the writer's rejection labels are fewer.
const maxEntry, maxLabels = 200, 64

// Settings are fleet-up's, which the readiness gate checks, and the writes in flight per stream: 8 each by default, at least 1.
type Settings struct {
	fleetup.Settings `yaml:",inline"`
	Writes           struct {
		Concurrency Concurrency `yaml:"concurrency"`
	} `yaml:"writes"`
}

// ParseSettings reads the config's settings mapping as fleet-up does, with the write concurrency.
func ParseSettings(n yaml.Node) (Settings, error) {
	s := Settings{Settings: fleetup.DefaultSettings()}
	s.Writes.Concurrency = DefaultConcurrency
	if err := fleetup.DecodeSettings(n, &s); err != nil {
		return Settings{}, err
	}
	if c := s.Writes.Concurrency; min(c.Rack, c.Host, c.Switch, c.PowerShelf) < 1 {
		return Settings{}, &config.Error{Field: "settings.writes.concurrency", Reason: "every stream needs at least 1 write in flight"}
	}
	return s, nil
}

// Summarizer is the metrics store's stage summaries: store.Client.
type Summarizer interface {
	Summarize(ctx context.Context, until time.Time, f probeapi.Filter, p probeapi.SummaryParams) (probeapi.SummaryResponse, error)
}

// Test is the ingestion run. check is readiness.Run; a test swaps it.
type Test struct {
	cfg   *config.Config
	s     Settings
	deps  readiness.Deps
	probe probe.Probe
	dial  func(apiURL string) (ForgeWriter, error)
	check func(context.Context, readiness.Deps, time.Time) readiness.Report
}

// New is the ingestion run over the readiness deps, which also give the store and the pods; p pages the end reads and dial connects the nico-api writer.
func New(cfg *config.Config, s Settings, d readiness.Deps, p probe.Probe, dial func(string) (ForgeWriter, error)) *Test {
	return &Test{cfg, s, d, p, dial, readiness.Run}
}

// Production is the run over fleet-up's live wiring, writing through nico-api with the mounted client certificate.
func Production(cfg *config.Config, s Settings, d readiness.Deps) (*Test, error) {
	p, ok := d.Probe.(probe.Probe)
	if !ok {
		return nil, errors.New("the readiness probe cannot page units")
	}
	return New(cfg, s, d, p, func(url string) (ForgeWriter, error) {
		c, err := forge.Dial(url, s.Nico())
		if err != nil {
			return nil, err
		}
		return c, nil
	}), nil
}

func (*Test) Name() string              { return "ingestion" }
func (*Test) LockKind() lock.HolderKind { return lock.HolderIngestion }
func (*Test) Writes() bool              { return true }

// ingest is one run's state, from the gate to the result.
type ingest struct {
	t         *Test
	rc        run.RunCtx
	inv       *inventory.SiteInventory
	plan      WritePlan
	recs      map[WriteKey]Record
	stages    []Stage
	invs      []countInvariant
	cpBefore  map[string]podState
	matBefore map[string]podState
	store     Summarizer
	res       Results
	seen      *observed
	d         detail
}

// Run gates on readiness, then sends the plan and observes the stages to the deadline, then reads the end state. Nothing is sent unless the site is ready:
// readiness, the write plan, the invariant and pod baselines, the store and the result size must all pass before the deadline. A dry run journals the plan
// and sends nothing. A send error or a cancel keeps every outcome and what the end reads still allow, with the error.
func (t *Test) Run(rc run.RunCtx, w *writer.Writer) (result.Outcome, error) {
	// The work ends at the wall deadline, on the monotonic clock: no admission, new send or stage count past it.
	workEnd := rc.Clock.Instant().Add(rc.Deadline.Sub(rc.Clock.Now()))
	window := time.Duration(t.cfg.Timeouts.ReadinessSeconds) * time.Second
	deadline := rc.Clock.Instant().Add(window - min(window/10, 5*time.Second))
	if workEnd.Before(deadline) {
		deadline = workEnd
	}
	d := t.deps
	d.Now = rc.Clock.Now
	rep := t.check(rc.Ctx, d, deadline)
	if err := rc.Ctx.Err(); err != nil {
		return result.Outcome{}, err
	}
	in := &ingest{t: t, rc: rc, inv: &rep.Inventory, stages: Stages(rep.Inventory.N), invs: countInvariants()}
	in.d.Checks, in.d.Plan = rep.Checks, rep.Inventory.N
	if !in.gate(deadline, workEnd, rep.Ready()) {
		var reasons []string
		for _, c := range in.d.Checks {
			if !c.OK {
				reasons = append(reasons, c.Name+": "+c.Detail)
			}
		}
		raw, _ := in.d.raw()
		return result.Found(result.NotReady, reasons, raw), nil
	}
	in.d.GoAt = &jsonx.Time{Time: rc.Clock.Now()}
	conc := t.s.Writes.Concurrency
	if rc.DryRun {
		in.res = Dispatch(rc.Ctx, w, nil, in.plan, in.recs, conc, rc.Deadline, true)
		in.d.Sends, _ = sends(in.res)
		raw, _ := in.d.raw()
		n := 0
		for _, c := range in.d.Sends {
			n += c[Planned]
		}
		if n < len(in.res.Outcomes) { // Go needs every planned key durably planned
			return result.Found(result.NotReady, []string{fmt.Sprintf("the dry run planned %d of %d writes before it stopped", n, len(in.res.Outcomes))}, raw), in.res.Err
		}
		return result.Found(result.Go, nil, raw), in.res.Err
	}
	url, _ := in.inv.CarbideAPIURL()
	f, err := t.dial(url)
	if err != nil {
		return result.Outcome{}, err
	}
	if c, ok := f.(io.Closer); ok {
		defer func() { _ = c.Close() }()
	}
	obs := observe(rc.Ctx, t.probe, in.stages, in.invs, time.Duration(t.cfg.Probe.PollSeconds)*time.Second, workEnd, rc.ProbeReadEnd)
	in.res = Dispatch(rc.Ctx, w, f, in.plan, in.recs, conc, rc.Deadline, false)
	timed, until := in.res.Err == nil, time.Time{} // a zero until stops the observer at once
	if timed {
		until = rc.Deadline
	}
	seen := obs.stop(until)
	in.seen = &seen
	out := in.end(timed)
	if in.res.Err != nil {
		return out, in.res.Err
	}
	return out, rc.Ctx.Err()
}

// gate checks the write plan, then, on a ready site, the invariant baselines read fresh and 0 (a missing one is never 0), the MAT and control-plane pod
// baselines, the store's summaries, the largest result the plan can give and the work deadline. Each failure is a failed check.
func (in *ingest) gate(deadline, workEnd time.Time, ready bool) bool {
	fail := func(name, why string) { in.d.Checks = append(in.d.Checks, readiness.Check{Name: name, Detail: why}) }
	var err error
	if in.plan, in.recs, err = Records(in.inv); err != nil {
		fail("writePlan", err.Error())
	}
	if !ready || err != nil {
		return false
	}
	ctx, cancel := context.WithDeadline(in.rc.Ctx, deadline)
	defer cancel()
	for i := range in.invs {
		if in.invs[i].Base, err = freshCount(ctx, in.t.probe, in.invs[i].Filter); err != nil {
			fail("invariantBaseline", in.invs[i].Name+": "+why(err))
		} else if in.invs[i].Base != 0 {
			fail("invariantBaseline", fmt.Sprintf("%s: %d present, 0 required on a greenfield site", in.invs[i].Name, in.invs[i].Base))
		}
	}
	mat, cp := capture(ctx, in.t.deps.Pods, in.matSelector()), capture(ctx, in.t.deps.Pods, in.t.cfg.ControlPlane)
	if in.matBefore, in.cpBefore = mat.Pods, cp.Pods; mat.Fail != "" {
		fail("matPods", mat.Fail)
	}
	if cp.Fail != "" {
		fail("controlPlanePods", cp.Fail)
	}
	if in.t.deps.Store == nil {
		fail("metricsStore", "the store is not checked")
	} else if sp, err := in.t.deps.Store(ctx); err != nil {
		fail("metricsStore", "store client: "+err.Error())
	} else if in.store, _ = sp.Client.(Summarizer); in.store == nil {
		fail("metricsStore", "the store client gives no stage summaries")
	}
	if n := in.worstSize(); n > int(in.t.cfg.ResultMaxBytes) {
		fail("resultSize", fmt.Sprintf("the largest result of this plan takes %d bytes, over resultMaxBytes %d", n, in.t.cfg.ResultMaxBytes))
	}
	if !in.rc.Clock.Instant().Before(workEnd) {
		fail("workDeadline", "the run deadline passed before the first write")
	}
	return !slices.ContainsFunc(in.d.Checks, func(c readiness.Check) bool { return !c.OK })
}

func (in *ingest) matSelector() []config.PodSelector {
	return []config.PodSelector{{Namespace: in.t.cfg.Namespace, LabelSelector: in.t.s.Mat.Selector}}
}

// podCapture is every serving pod the selectors match, keyed namespace/pod, and why a listing failed.
type podCapture struct {
	Pods map[string]podState
	Fail string
}

// podState is a pod's identity and restart count: a pod recreated under its name has a new UID.
type podState struct {
	UID      string
	Restarts uint32
}

func capture(ctx context.Context, pods inventory.PodLister, sels []config.PodSelector) podCapture {
	c, fails := podCapture{Pods: map[string]podState{}}, []string{}
	for _, s := range sels {
		list, err := pods.ListPods(ctx, s.Namespace, s.LabelSelector)
		if err != nil {
			fails = append(fails, fmt.Sprintf("listing %s in %s: %s", s.LabelSelector, s.Namespace, err))
		}
		for _, p := range list {
			if readiness.Serving(p) {
				c.Pods[s.Namespace+"/"+p.Name] = podState{p.UID, p.Restarts}
			}
		}
	}
	c.Fail = strings.Join(fails, "; ")
	return c
}

var (
	errReadsCancelled = errors.New("the run was cancelled during the end reads")
	errPastReadEnd    = errors.New("the end reads passed the probe read end")
)

// guard bounds every end read by the run's cancel and the probe read end, fixed once: a read is refused once either passed, and a read that answers past
// either is not taken.
type guard struct {
	ctx context.Context
	end time.Time
}

func (g guard) refused() error {
	switch {
	case g.ctx.Err() != nil:
		return errReadsCancelled
	case !time.Now().Before(g.end):
		return errPastReadEnd
	}
	return nil
}

func guarded[T any](g guard, read func(context.Context) (T, error)) (T, error) {
	var zero T
	if err := g.refused(); err != nil {
		return zero, err
	}
	ctx, cancel := context.WithDeadline(g.ctx, g.end)
	defer cancel()
	v, err := read(ctx)
	if gerr := g.refused(); gerr != nil {
		return zero, gerr
	}
	return v, err
}

// why is a failed read as fixed text, cut: the probe's own detail for an unusable read, else the error's kind. Every dependency's error is free of reply text.
func why(err error) string {
	if pe := (*probe.Error)(nil); errors.As(err, &pe) && pe.Kind == probe.Unavailable {
		return cut(pe.Detail)
	}
	return cut(err.Error())
}

// cut is s as the result keeps it: at most maxEntry bytes, each byte JSON escapes or that is not ASCII as '?'.
func cut(s string) string {
	b := []byte(s[:min(len(s), maxEntry)])
	for i, c := range b {
		if c < ' ' || c > '~' || strings.IndexByte(`"\<>&`, c) >= 0 {
			b[i] = '?'
		}
	}
	return string(b)
}

// end is the run's result from fresh reads after the observer, correctness first: every planned key's outcome, the invariants, the host gate and the pods, then,
// when timed, each stage's summary, one at a time. Each read is guarded on its own: a cut read is unavailable evidence and what was read before stays.
func (in *ingest) end(timed bool) result.Outcome {
	g, p := guard{in.rc.Ctx, in.rc.ProbeReadEnd}, in.t.probe
	units := func(f probeapi.Filter) endRead {
		u, err := guarded(g, func(ctx context.Context) ([]probeapi.Unit, error) { return probe.AllUnits(ctx, p, g.end, f, nil) })
		if err != nil {
			return endRead{err: why(err)}
		}
		return endRead{units: u}
	}
	end := EndUnits{Records: map[probeapi.RecordType]endRead{}, Devices: map[probeapi.UnitType]endRead{}}
	for _, k := range kinds {
		end.Records[k.record] = units(probeapi.Filter{Type: probeapi.UnitTypeExpectedRecord, RecordType: &k.record})
		end.Devices[k.device] = units(probeapi.Filter{Type: k.device})
	}
	end.Devices[probeapi.UnitTypeDpu] = units(probeapi.Filter{Type: probeapi.UnitTypeDpu})
	rec := reconcile(in.plan, in.inv, in.res.Outcomes, end)

	invs, names := []result.InvariantResult{}, []string{}
	in.d.IdentityMismatches = map[string]*uint64{}
	for _, inv := range in.invs {
		n, err := guarded(g, func(ctx context.Context) (uint64, error) { return freshCount(ctx, p, inv.Filter) })
		reason := ""
		if err != nil {
			reason = why(err)
		}
		invs, names = append(invs, countResult(inv, *in.seen, n, reason)), append(names, inv.Name)
		if inv.Filter.Type == probeapi.UnitTypeIdentityMismatch {
			in.d.IdentityMismatches[inv.Name] = nil // unknown, never 0
			if err == nil {
				in.d.IdentityMismatches[inv.Name] = &n
			}
		}
	}
	if _, err := guarded(g, func(ctx context.Context) (probeapi.CountResponse, error) { return p.Count(ctx, hostGate()) }); err != nil {
		in.d.HostGate = "unknown: " + why(err)
	}
	pods := func(sels []config.PodSelector) podCapture {
		c, err := guarded(g, func(ctx context.Context) (podCapture, error) { return capture(ctx, in.t.deps.Pods, sels), nil })
		if err != nil {
			c.Fail = err.Error()
		}
		return c
	}
	// A MAT pod is judged by matRestarts alone, also when a control-plane selector matches it.
	mat, cp := pods(in.matSelector()), pods(in.t.cfg.ControlPlane)
	for _, m := range []map[string]podState{in.matBefore, mat.Pods} {
		for pod := range m {
			delete(in.cpBefore, pod)
			delete(cp.Pods, pod)
		}
	}
	invs = append(invs, unexpectedUnits(rec), restartResult("matRestarts", in.matBefore, mat.Pods, mat.Fail),
		restartResult("controlPlaneRestarts", in.cpBefore, cp.Pods, cp.Fail))
	names = append(names, "unexpectedUnits", "matRestarts", "controlPlaneRestarts")

	stages, stop := make([]result.StageResult, 0, len(in.stages)), in.seen.At
	if in.rc.Deadline.Before(stop) { // a crossing past the deadline is not the run's: the store's final scrape confirms the window, never extends it
		stop = in.rc.Deadline
	}
	in.d.StageReadErrors = []string{}
	for _, st := range in.stages {
		sd := stageDetail{ID: st.ID}
		var sum *probeapi.SummaryResponse
		w := in.res.Windows[st.Stream]
		if !w.T0.IsZero() {
			sd.T0 = &jsonx.Time{Time: w.T0}
		}
		if !w.End.IsZero() {
			sd.WindowEnd = &jsonx.Time{Time: w.End}
		}
		switch {
		case st.N == 0:
		case !timed:
			sd.Unavailable = "not summarized: the sends ended on an error"
		case sd.T0 == nil:
			sd.Unavailable = "no T0: the stream was never dispatched"
		case w.End.Before(w.T0): // an unknown end is never a zero window
			sd.Unavailable = "no usable window end: no accepted or unclear write"
		default:
			params := probeapi.SummaryParams{Start: *sd.T0, End: jsonx.Time{Time: stop}, N: st.N, AddWindowSeconds: w.End.Sub(w.T0).Seconds()}
			s, err := guarded(g, func(ctx context.Context) (probeapi.SummaryResponse, error) {
				return in.store.Summarize(ctx, g.end, st.Filter, params)
			})
			if err != nil {
				sd.Unavailable = "summary: " + why(err)
				in.d.StageReadErrors = append(in.d.StageReadErrors, string(st.ID)+" summary: "+why(err))
			} else {
				sum = &s
			}
		}
		status := timing(sum)
		if sum == nil && in.seen.Failed[st.ID] > 0 { // an unsummarized stage keeps the failures the observer saw
			status = result.StageFailed
		}
		sd.Summary = sum
		stages = append(stages, stageResult(st, rec.Tallies[st.ID], status))
		in.d.Stages = append(in.d.Stages, sd)
	}

	verdict := result.Evaluate(result.Admitted{Stages: stages, Planned: len(in.stages), Invariants: names}, invs)
	in.d.Sends, in.d.Uncertain = sends(in.res)
	in.d.ReplyLatency, in.d.Observed, in.d.Outcomes, in.d.Tallies = replyLatency(in.res), in.seen, outcomes(in.stages, rec), rec.Tallies
	in.d.Unexpected, in.d.Pending, in.d.ReadErrors = capped(rec.Unexpected), capped(rec.Pending), capped(rec.ReadErrors)
	// The reasons name each stage and invariant short of a clean pass in fixed text, each cut: the result keeps them whole.
	var reasons []string
	for _, s := range stages {
		if s.Status != result.StageDone && s.Status != result.StageNotApplicable {
			reasons = append(reasons, fmt.Sprintf("stage %s: %s %d/%d", s.ID, s.Status, s.Units.Done, s.Units.N))
		}
	}
	for _, i := range invs {
		if !i.Held || i.Checks == 0 {
			reasons = append(reasons, cut("invariant "+i.Name+": "+*cmp.Or(i.Detail, new("unchecked"))))
		}
	}
	raw, ok := in.d.raw()
	if !ok {
		verdict, reasons = result.NotClean(verdict), append(reasons, "the ingestion detail did not encode")
	}
	out := result.Found(verdict, reasons, raw)
	out.Stages, out.Invariants = stages, invs
	return out
}

// detail is detail.ingestion of the result. Outcomes groups every planned unit not done and Uncertain lists every unclear send, both uncapped; the other
// lists keep their first 100 entries, each cut, and a count.
type detail struct {
	Checks             []readiness.Check         `json:"checks"`
	Plan               inventory.TypeCounts      `json:"plan"`
	GoAt               *jsonx.Time               `json:"goAt,omitempty"`
	Sends              map[Stream]map[Status]int `json:"sends,omitempty"`
	ReplyLatency       map[Stream]latency        `json:"replyLatency,omitempty"`
	HostGate           string                    `json:"hostGate,omitempty"`
	Stages             []stageDetail             `json:"stages,omitempty"`
	Observed           *observed                 `json:"observed,omitempty"`
	Outcomes           map[string][]WriteKey     `json:"outcomes,omitempty"`
	Uncertain          []WriteKey                `json:"uncertain,omitempty"`
	Tallies            map[result.StageID]*Tally `json:"tallies,omitempty"`
	IdentityMismatches map[string]*uint64        `json:"identityMismatches,omitempty"`
	Unexpected         []string                  `json:"unexpected,omitempty"`
	Pending            []string                  `json:"pending,omitempty"`
	ReadErrors         []string                  `json:"readErrors,omitempty"`
	StageReadErrors    []string                  `json:"stageReadErrors,omitempty"`
}

// stageDetail is a stage's timing: its stream's window, the store's summary (when the ready count crossed each fixed target) or why there is none.
type stageDetail struct {
	ID          result.StageID            `json:"id"`
	T0          *jsonx.Time               `json:"t0"`
	WindowEnd   *jsonx.Time               `json:"windowEnd"`
	Summary     *probeapi.SummaryResponse `json:"summary"`
	Unavailable string                    `json:"unavailable,omitempty"`
}

// latency is the reply time of a stream's accepted writes, first dispatch to reply: nearest-rank p50 and p95, and the longest.
type latency struct {
	Accepted int      `json:"accepted"`
	P50      *float64 `json:"p50Seconds"`
	P95      *float64 `json:"p95Seconds"`
	Max      *float64 `json:"maxSeconds"`
}

func (d *detail) raw() (json.RawMessage, bool) {
	b, err := json.Marshal(map[string]*detail{"ingestion": d})
	return b, err == nil
}

// sends counts each stream's keys by send status, rack groups included, and lists every unclear key, sorted: a Ready end does not show which run made its
// record.
func sends(res Results) (map[Stream]map[Status]int, []WriteKey) {
	out, unclear := map[Stream]map[Status]int{}, []WriteKey{}
	for k, o := range res.Outcomes {
		s, _, _ := strings.Cut(string(k), "/")
		if out[Stream(s)] == nil {
			out[Stream(s)] = map[Status]int{}
		}
		if out[Stream(s)][o.Status]++; o.Status == Unclear {
			unclear = append(unclear, k)
		}
	}
	slices.Sort(unclear)
	return out, unclear
}

func replyLatency(res Results) map[Stream]latency {
	secs := map[Stream][]float64{}
	for k, o := range res.Outcomes {
		if s, _, _ := strings.Cut(string(k), "/"); o.Status == Sent {
			secs[Stream(s)] = append(secs[Stream(s)], o.RepliedAt.Sub(o.SentAt).Seconds())
		}
	}
	out := map[Stream]latency{}
	for s, v := range secs {
		slices.Sort(v)
		q := func(f float64) *float64 { return &v[max(int(math.Ceil(f*float64(len(v))))-1, 0)] }
		out[s] = latency{len(v), q(0.5), q(0.95), &v[len(v)-1]}
	}
	return out
}

// outcomes groups each planned unit not done by "outcome:cause" of its most severe stage outcome, failed before unknown, then the first in stage order. A DPU is
// its own unit, <host key>#<n>, apart from its host. The counts per stage are the tallies'.
func outcomes(stages []Stage, rec *Reconciled) map[string][]WriteKey {
	worst := map[string]Outcome{}
	for _, st := range stages {
		for u, o := range rec.Units[st.ID] {
			if w := worst[u]; o.Outcome != "done" && (w.Outcome == "" || w.Outcome != "failed" && o.Outcome == "failed") {
				worst[u] = o
			}
		}
	}
	out := map[string][]WriteKey{}
	for _, k := range slices.Sorted(maps.Keys(worst)) {
		l := worst[k].Outcome + ":" + worst[k].Cause
		out[l] = append(out[l], WriteKey(k))
	}
	return out
}

// capped is l's first 100 entries, each cut, and a count of the rest.
func capped(l []string) []string {
	out := []string{}
	for i, s := range l {
		if i == 100 {
			return append(out, fmt.Sprintf("and %d more", len(l)-100))
		}
		out = append(out, cut(s))
	}
	return out
}

// worstSize is the encoded size of the largest result.json this plan can give: every unit not done under the longest label, every send unclear, every
// stage summarized and every invariant broken, each kept text at its cut and each list at its cap, in the runner's record with its own reasons
// and the test's: one per stage and invariant, each at its cut, and the encode's.
func (in *ingest) worstSize() int {
	pad, many, num := strings.Repeat("x", maxEntry), uint64(math.MaxUint64), -math.MaxFloat64
	list, at, cfg := slices.Repeat([]string{pad}, 101), &jsonx.Time{Time: time.Date(9999, 12, 31, 23, 59, 59, 999999999, time.UTC)}, in.t.cfg
	text, d := strings.Join(list, "; "), in.d
	seen := observed{map[string]string{}, map[string]uint64{}, map[string]string{}, map[result.StageID]uint64{}, map[result.StageID]uint64{}, time.Time{}}
	d.GoAt, d.HostGate, d.Observed, d.Uncertain, d.Unexpected, d.Pending, d.ReadErrors = at, pad, &seen, in.plan.keys(), list, list, list
	d.Sends, d.ReplyLatency, d.Tallies, d.IdentityMismatches = map[Stream]map[Status]int{}, map[Stream]latency{}, map[result.StageID]*Tally{}, map[string]*uint64{}
	units, slot := in.plan.UnitKeys(), map[WriteKey]int{} // every unit not done: the plan's keys and each planned DPU, <host key>#<n>
	for _, p := range in.inv.Pods {
		for _, dpu := range p.Records.DPUs {
			h := DeviceKey(Host, dpu.ParentBmcMac)
			units, slot[h] = append(units, WriteKey(fmt.Sprintf("%s#%d", h, slot[h]))), slot[h]+1
		}
	}
	d.Outcomes = map[string][]WriteKey{pad: units}
	for i := range maxLabels - 1 {
		d.Outcomes[fmt.Sprint(i, pad)] = []WriteKey{}
	}
	for _, s := range []Stream{"rackGroup", Rack, Host, Switch, PowerShelf} {
		d.Sends[s], d.ReplyLatency[s] = map[Status]int{}, latency{math.MaxInt, &num, &num, &num}
		for _, st := range []Status{Sent, Planned, ExistedBeforeRun, Rejected, Unclear, NotSent, GroupNotAccepted, RackNotAccepted} {
			d.Sends[s][st] = math.MaxInt
		}
	}
	sum := probeapi.SummaryResponse{Family: probeapi.Family(pad), Start: *at, End: *at, N: many, FinalCount: &many, FinalSweepAt: at, MaxSweepGapSeconds: &num,
		MaxSweepDurationSeconds: &num, AddWindowSeconds: num, TimingErrorSeconds: &num, SweepsSeen: math.MaxUint32, WorstCountAfterFirstCrossing: &many}
	for range probeapi.FixedQuantiles {
		sum.Crossings = append(sum.Crossings, probeapi.Crossing{Q: num, Target: many, ReachedAt: at, Seconds: &num, WorstCountAfter: &many})
	}
	out := result.RunResult{Test: in.t.Name(), RunID: cfg.RunID, ExecutionID: cfg.ExecutionID, DryRun: new(false), Outcome: result.Found(result.Verdict(pad), nil, nil),
		Reasons: slices.Repeat([]string{pad}, 8), Clock: result.RunClock{StartedAt: *at, Deadline: at, EndedAt: *at, ClockSyncAssumption: result.ClockSyncAssumption},
		Lock: result.LockResult{Name: cfg.Site.LockName, Kind: pad, AcquiredAt: at, LostAt: at, FinalState: &pad}, Journal: result.JournalSummary{Ops: &many, Attempts: &many, Unclear: &many},
		Result: result.ResultRef{Claim: cfg.Journal.Claim, Path: cfg.Journal.Dir + pad}}
	for _, st := range in.stages {
		d.Stages = append(d.Stages, stageDetail{st.ID, at, at, &sum, pad})
		d.StageReadErrors, d.Tallies[st.ID] = append(d.StageReadErrors, string(st.ID)+" summary: "+pad), &Tally{many, many, many, many, many}
		seen.Count[st.ID], seen.Failed[st.ID] = many, many
		out.Stages = append(out.Stages, result.StageResult{ID: st.ID, Status: result.StageStatus(pad), Units: result.UnitCounts{N: many, Done: many, Failed: many, Unknown: many, FailedPct: &num}})
	}
	for _, name := range []string{"ambiguousLinks", "devicesWithoutRecord", "nicoInstances", "unexpectedUnits", "matRestarts", "controlPlaneRestarts"} {
		seen.Breaks[name], seen.Checks[name], seen.Unread[name], d.IdentityMismatches[name] = pad, many, pad, &many
		out.Invariants = append(out.Invariants, result.InvariantResult{Name: name, Checks: many, Detail: &text})
	}
	out.Reasons = append(out.Reasons, slices.Repeat([]string{pad}, len(out.Stages)+len(out.Invariants)+1)...)
	out.Detail, _ = d.raw()
	b, err := jsonx.Encode(&out)
	if err != nil {
		return math.MaxInt
	}
	return len(b)
}
