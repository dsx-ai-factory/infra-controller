// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"cmp"
	"context"
	"fmt"
	"maps"
	"slices"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
)

// observed is what the observer saw during the run: invariant breaks, which stand whatever a later read shows, the fresh checks of each invariant and why its
// last read could not be used, and each stage's last fresh count read before the deadline and failed count (a stage never read fresh has none, never 0).
// At is when it stopped.
type observed struct {
	Breaks map[string]string         `json:"breaks"`
	Checks map[string]uint64         `json:"checks"`
	Unread map[string]string         `json:"unread"`
	Count  map[result.StageID]uint64 `json:"count"`
	Failed map[result.StageID]uint64 `json:"failed"`
	At     time.Time                 `json:"-"`
}

// observer polls the probe every poll until it is finished or cancelled, with every read bounded by the earlier of workEnd and until, and no stage count taken from a read that answers at or
// after workEnd: a crossing past the deadline is not the run's.
type observer struct {
	p      probe.Probe
	stages []Stage
	invs   []countInvariant
	poll   time.Duration
	cutoff time.Time // the work deadline, monotonic
	finish chan time.Time
	cancel context.CancelFunc
	done   chan struct{}
	seen   observed
}

func observe(ctx context.Context, p probe.Probe, stages []Stage, invs []countInvariant, poll time.Duration, workEnd, until time.Time) *observer {
	if workEnd.Before(until) { // the polling reads end with the work, never in the evidence budget after it
		until = workEnd
	}
	ctx, cancel := context.WithDeadline(ctx, until)
	o := &observer{p: p, stages: stages, invs: invs, poll: poll, cutoff: workEnd, finish: make(chan time.Time, 1), cancel: cancel, done: make(chan struct{}),
		seen: observed{map[string]string{}, map[string]uint64{}, map[string]string{}, map[result.StageID]uint64{}, map[result.StageID]uint64{}, time.Time{}}}
	go o.run(ctx)
	return o
}

// stop finishes the observer at deadline, or at once when deadline is zero, and returns what it saw. Finished, it polls on until every stage reached N or the
// deadline passed; the run's cancel and the read end stop it sooner.
func (o *observer) stop(deadline time.Time) observed {
	if deadline.IsZero() {
		o.cancel()
	} else {
		o.finish <- deadline
	}
	<-o.done
	o.cancel()
	return o.seen
}

func (o *observer) run(ctx context.Context) {
	defer close(o.done)
	tick := time.NewTicker(o.poll)
	defer tick.Stop()
	var due <-chan time.Time
	finishing := false
	for {
		o.check(ctx)
		reached := !slices.ContainsFunc(o.stages, func(s Stage) bool { return o.seen.Count[s.ID] < s.N })
		if finishing && reached {
			break
		}
		select {
		case <-ctx.Done():
		case <-due:
		case d := <-o.finish:
			finishing, due = true, time.After(time.Until(d))
			continue
		case <-tick.C:
			continue
		}
		break
	}
	o.seen.At = time.Now()
}

// check reads each invariant not yet broken and each stage not yet at N, each from a fresh sweep.
func (o *observer) check(ctx context.Context) {
	for _, inv := range o.invs {
		if _, broken := o.seen.Breaks[inv.Name]; broken {
			continue
		}
		switch n, err := freshCount(ctx, o.p, inv.Filter); {
		case err != nil:
			o.seen.Unread[inv.Name] = why(err)
		case n != inv.Base:
			o.seen.Breaks[inv.Name] = fmt.Sprintf("count %d during the run, %d before it", n, inv.Base)
			o.seen.Checks[inv.Name]++
		default:
			o.seen.Checks[inv.Name]++
		}
	}
	for _, s := range o.stages {
		if n, seen := o.seen.Count[s.ID]; s.N == 0 || seen && n >= s.N {
			continue
		}
		if n, err := freshCount(ctx, o.p, s.Filter); err == nil && time.Now().Before(o.cutoff) {
			o.seen.Count[s.ID] = n
		}
		if s.Failed != nil {
			if n, err := freshCount(ctx, o.p, *s.Failed); err == nil {
				o.seen.Failed[s.ID] = n
			}
		}
	}
}

// freshCount is a count from a sweep at most two intervals old: a failed or stale read is no count, never 0.
func freshCount(ctx context.Context, p probe.Probe, f probeapi.Filter) (uint64, error) {
	r, err := p.Count(ctx, f)
	switch {
	case err != nil:
		return 0, err
	case !probe.Fresh(r.SweepInfo):
		return 0, fmt.Errorf("the %s sweep is %.0f s old", r.Family, r.SweepAgeSeconds)
	}
	return r.Count, nil
}

func held(name string, checks uint64) result.InvariantResult {
	return result.InvariantResult{Name: name, Held: true, Checks: checks}
}

func broke(name string, checks uint64, detail string) result.InvariantResult {
	return result.InvariantResult{Name: name, Checks: max(checks, 1), Detail: &detail}
}

// unresolved is an invariant without a usable check: held, no check, and why. It keeps the run from passing without claiming a break.
func unresolved(name, why string) result.InvariantResult {
	why = "unavailable: " + why
	return result.InvariantResult{Name: name, Held: true, Detail: &why}
}

// countResult is a count invariant's result: a break the observer saw stands; else a fresh end read off the baseline breaks it. A failed, stale or cut end read,
// or a run that never checked it, leaves it unresolved.
func countResult(inv countInvariant, seen observed, end uint64, why string) result.InvariantResult {
	checks := seen.Checks[inv.Name]
	switch b, broken := seen.Breaks[inv.Name]; {
	case broken:
		return broke(inv.Name, checks, b)
	case why != "":
		return unresolved(inv.Name, "end read: "+why)
	case end != inv.Base:
		return broke(inv.Name, checks+1, fmt.Sprintf("count %d at the end, %d before the run", end, inv.Base))
	case checks == 0:
		return unresolved(inv.Name, "never checked during the run: "+cmp.Or(seen.Unread[inv.Name], "never read"))
	}
	return held(inv.Name, checks+1)
}

// unexpectedUnits is broken by a proven extra record or device, whatever else is missing. Otherwise a failed end read leaves the census incomplete and a pending
// device is not proven extra: unresolved, never held.
func unexpectedUnits(r *Reconciled) result.InvariantResult {
	var open []string
	if len(r.ReadErrors) > 0 {
		open = append(open, fmt.Sprintf("%d end reads failed, first: %s", len(r.ReadErrors), cut(r.ReadErrors[0])))
	}
	if len(r.Pending) > 0 {
		open = append(open, fmt.Sprintf("pending: %d devices not proven extra, first: %s", len(r.Pending), cut(r.Pending[0])))
	}
	switch {
	case len(r.Unexpected) > 0:
		first := fmt.Sprintf("%d records or devices the plan does not account for, first: %s", len(r.Unexpected), cut(r.Unexpected[0]))
		return broke("unexpectedUnits", 1, strings.Join(append([]string{first}, open...), "; "))
	case len(open) > 0:
		return unresolved("unexpectedUnits", strings.Join(open, "; "))
	}
	return held("unexpectedUnits", 1)
}

// restartResult judges an end pod capture against its baseline, both keyed namespace/pod: a pod added, replaced (a new UID) or restarted breaks it, and so
// does one removed when every listing answered. A failed capture, or no pod to watch, leaves it unresolved, never zero changes.
func restartResult(name string, before, after map[string]podState, failure string) result.InvariantResult {
	var changes []string
	for _, pod := range slices.Sorted(maps.Keys(before)) {
		switch n, ok := after[pod]; {
		case !ok && failure == "":
			changes = append(changes, "pod "+pod+" removed")
		case ok && n.UID != before[pod].UID:
			changes = append(changes, "pod "+pod+" replaced")
		case ok && n.Restarts != before[pod].Restarts:
			changes = append(changes, fmt.Sprintf("pod %s restarts %d to %d", pod, before[pod].Restarts, n.Restarts))
		}
	}
	for _, pod := range slices.Sorted(maps.Keys(after)) {
		if _, ok := before[pod]; !ok {
			changes = append(changes, "pod "+pod+" added")
		}
	}
	switch {
	case len(changes) > 0:
		return broke(name, 1, strings.Join(capped(changes), "; "))
	case failure != "":
		return unresolved(name, failure)
	case len(before) == 0:
		return unresolved(name, "no pod was captured")
	}
	return held(name, 1)
}
