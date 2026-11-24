// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/clustertest"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// cfgRun sets the evidence window. 1 second reaches the drain and watchdog quickly; 6 hours (6*3600) puts the absolute
// maximum hours away, so only the teardown or outside-cancel bound can exit a stall in time.
func cfgRun(dir string, wholeRunSeconds uint64) *config.Config {
	c := cfg(dir)
	c.Timeouts = config.Timeouts{WholeRunSeconds: wholeRunSeconds, ReadinessSeconds: 0, FinalizeSeconds: 0}
	return c
}

// seededCM is an in-memory ConfigMaps with the empty result ConfigMap the chart makes.
func seededCM() *cmFake {
	cm := newCM()
	cm.seed("r1-result", nil)
	return cm
}

func write(ctx context.Context, w *writer.Writer, key string, idempotent bool, a writer.Attempt) {
	_, _ = writer.Send(ctx, w, writer.Call{
		WriteCall: journal.WriteCall{Scope: "test", Kind: "k", Key: key, Idempotent: idempotent},
		Retry:     writer.DefaultRetry,
	}, func(context.Context) writer.Attempt { return a })
}

// The child's budgets, shared with the parent's ceiling so removing or delaying the watchdog fails promptly.
const (
	childDrain    = 100 * time.Millisecond
	childShutdown = 500 * time.Millisecond
	// childCeiling bounds the whole child: the budgets plus allowance for start/scheduling. The watchdog fires near 1.5s.
	childCeiling = time.Second + childDrain + childShutdown + 8*time.Second
)

// TestShutdownBoundExitsOnStall proves a watchdog exits the process with ExitNotRun, inside a hard ceiling and with no
// clean marker, for every stall; the long-window rows prove the teardown and cancel bounds, not just the absolute one.
func TestShutdownBoundExitsOnStall(t *testing.T) {
	if sc := os.Getenv("F1_STALL_SCENARIO"); sc != "" {
		runStallChild(sc, os.Getenv("F1_STALL_DIR"))
		return // unreachable: the watchdog exits the process
	}
	for _, sc := range []string{"startup", "callback", "journal", "persist-long", "cancel-create"} {
		t.Run(sc, func(t *testing.T) {
			dir := t.TempDir()
			ctx, cancel := context.WithTimeout(context.Background(), childCeiling)
			defer cancel()
			cmd := exec.CommandContext(ctx, os.Args[0], "-test.run=^TestShutdownBoundExitsOnStall$", "-test.v")
			cmd.Env = append(os.Environ(), "F1_STALL_SCENARIO="+sc, "F1_STALL_DIR="+dir)
			out, err := cmd.CombinedOutput()
			if ctx.Err() != nil {
				t.Fatalf("the watchdog did not exit the child within %s (it hung): %v\n%s", childCeiling, ctx.Err(), out)
			}
			var ee *exec.ExitError
			if !errors.As(err, &ee) {
				t.Fatalf("child did not exit with an error (a stall must exit): err=%v\n%s", err, out)
			}
			if ee.ExitCode() != result.ExitNotRun {
				t.Fatalf("child exit = %d, want %d\n%s", ee.ExitCode(), result.ExitNotRun, out)
			}
			if v := fileVerdict(dir, "r1"); result.IsSuccess(result.Verdict(v)) {
				t.Fatalf("a stall published a clean final marker: verdict %q", v)
			}
		})
	}
}

// runStallChild runs one stall scenario with a real watchdog (OnWatchdog nil) and small budgets, so a hang anywhere in
// the execution exits the process. It never returns on success; a return here fails the parent's exit check.
func runStallChild(scenario, dir string) {
	d := Deps{
		Config: cfgRun(dir, 1), Lock: &fakeLock{held: held(nil)}, CM: seededCM(), Clock: RealClock{},
		ShutdownBudget: childShutdown, DrainBudget: childDrain,
	}
	var test fakeTest
	switch scenario {
	case "startup": // the initial journal create stalls before any run timer or finish watchdog exists
		d.NewJournal = func(string, journal.Header) (*journal.Journal, error) { select {} }
		test = fakeTest{run: passes} // never reached
	case "callback": // ignores cancel, never sends, never returns
		test = fakeTest{run: func(RunCtx, *writer.Writer) (result.Outcome, error) { select {} }}
	case "journal": // the first journal append blocks forever, so the callback never returns
		d.JournalFault = func(*os.File, []byte) error { select {} }
		test = fakeTest{run: func(ctx RunCtx, w *writer.Writer) (result.Outcome, error) {
			write(ctx.Ctx, w, "key1", false, writer.Attempt{})
			return result.OutcomeOf(result.Passed), nil
		}}
	case "persist-long": // a quick success, then a blocked persistence, under a six-hour window: only the teardown bound can exit it
		d.Config = cfgRun(dir, 6*3600)
		d.CM = &blockingStatusCM{cmFake: seededCM(), block: make(chan struct{})}
		test = fakeTest{run: passes}
	case "cancel-create": // the initial create stalls, then the outside context is canceled: only the cancel bound can exit it
		d.Config = cfgRun(dir, 6*3600)
		d.NewJournal = func(string, journal.Header) (*journal.Journal, error) { select {} }
		octx, ocancel := context.WithCancel(context.Background())
		d.Outside = octx
		go func() { time.Sleep(200 * time.Millisecond); ocancel() }()
		test = fakeTest{run: passes} // never reached
	}
	Execute(d, test)
	os.Exit(0)
}

// fileVerdict is the stored result.json verdict for run in dir, or "" when no whole result was written.
func fileVerdict(dir, run string) string {
	data, err := os.ReadFile(result.ResultFor(dir, run).Path)
	if err != nil {
		return ""
	}
	var r struct {
		Verdict string `json:"verdict"`
	}
	if err := json.Unmarshal(data, &r); err != nil {
		return ""
	}
	return r.Verdict
}

// blockingStatusCM stalls the result-status write (the one carrying result.json data), but lets the claim through.
type blockingStatusCM struct {
	*cmFake
	block chan struct{}
}

func (b *blockingStatusCM) ReplaceConfigMap(ctx context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	if o.Data[result.ResultKey] != "" {
		<-b.block // the status write never completes
	}
	return b.cmFake.ReplaceConfigMap(ctx, o)
}

// TestUnsettledRunIsNeverPublishedClean proves a run whose writes cannot be proved settled (a lost non-idempotent
// reply or a failed append) holds the site for cleanup and publishes Unresolved in all three forms, not clean Passed.
func TestUnsettledRunIsNeverPublishedClean(t *testing.T) {
	rows := []struct {
		name    string
		fault   func(*os.File, []byte) error
		attempt writer.Attempt
	}{
		{"lost non-idempotent reply", nil, writer.Attempt{NoReply: true}},
		{"failed append breaks the journal", func(*os.File, []byte) error { return errors.New("append failed") }, writer.Attempt{}},
	}
	for _, r := range rows {
		t.Run(r.name, func(t *testing.T) {
			seq := &sequence{}
			h := held(seq)
			d := deps(t, seq, h)
			d.JournalFault = r.fault
			res := Execute(d, fakeTest{run: func(ctx RunCtx, w *writer.Writer) (result.Outcome, error) {
				write(ctx.Ctx, w, "key1", false, r.attempt)
				return result.OutcomeOf(result.Passed), nil
			}})
			if res.Lock.FinalState == nil || *res.Lock.FinalState != string(lock.NeedsCleanup) {
				t.Fatalf("final state = %v, want needsCleanup", res.Lock.FinalState)
			}
			if h.released != 1 {
				t.Errorf("site released %d times, want once", h.released)
			}
			if res.Verdict != result.Unresolved || res.ExitCode == 0 {
				t.Errorf("return = %q exit %d, want unresolved and non-zero", res.Verdict, res.ExitCode)
			}
			if v := d.CM.(*cmFake).written("r1-result")["verdict"]; string(v) != `"unresolved"` {
				t.Errorf("ConfigMap verdict = %s, want unresolved", v)
			}
			if v := fileVerdict(d.Config.Journal.Dir, d.Config.RunID); v != "unresolved" {
				t.Errorf("result file verdict = %q, want unresolved", v)
			}
		})
	}
}

// TestAbandonedRunRetainsSiteForCleanup proves a run dropped at the drain releases the real Lease as needsCleanup, so
// a second acquisition of it is refused: a dropped run's writes may still be unsettled.
func TestAbandonedRunRetainsSiteForCleanup(t *testing.T) {
	f := clustertest.New("ns")
	sl := &lock.SiteLock{Cluster: f, Name: "nico-scale-test-site-lock", Timing: lock.DefaultTiming}
	d := Deps{Config: cfgRun(t.TempDir(), 1), Lock: sl, CM: seededCM(), Clock: RealClock{}, DrainBudget: 50 * time.Millisecond}
	res := Execute(d, fakeTest{run: func(ctx RunCtx, _ *writer.Writer) (result.Outcome, error) {
		<-ctx.Ctx.Done()                   // the evidence bound cancels the run
		time.Sleep(200 * time.Millisecond) // but the callback keeps going past the drain: it is dropped
		return result.OutcomeOf(result.Passed), nil
	}})
	if res.Lock.FinalState == nil || *res.Lock.FinalState != string(lock.NeedsCleanup) {
		t.Fatalf("final state = %v, want needsCleanup", res.Lock.FinalState)
	}
	me := lock.HolderRecord{Kind: lock.HolderTest, RunID: "r2", ExecutionID: "r2-pod", State: lock.Running}
	if _, err := sl.Acquire(context.Background(), me); err == nil {
		t.Fatal("a second acquisition succeeded while the site was held for cleanup")
	}
}

// ctxCM honors its context: a canceled context fails every call, so a publication on the outside context would be lost.
type ctxCM struct{ *cmFake }

func (c ctxCM) GetConfigMap(ctx context.Context, name string) (*cluster.ConfigMapObject, error) {
	if err := ctx.Err(); err != nil {
		return nil, cluster.Error{Kind: cluster.Transport, Text: "context"}
	}
	return c.cmFake.GetConfigMap(ctx, name)
}

func (c ctxCM) ReplaceConfigMap(ctx context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	if err := ctx.Err(); err != nil {
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.Transport, Text: "context"}
	}
	return c.cmFake.ReplaceConfigMap(ctx, o)
}

// TestOutsideCancelStillPublishesAbort proves an outside-canceled run's abort is still recorded: publication runs on a
// detached context, so a ConfigMap adapter that honors the canceled outside context still stores the verdict.
func TestOutsideCancelStillPublishesAbort(t *testing.T) {
	seq := &sequence{}
	cm := ctxCM{seededCM()}
	cm.seq = seq
	outside, cancel := context.WithCancel(context.Background())
	d := Deps{Config: cfg(t.TempDir()), Lock: &fakeLock{held: held(seq)}, CM: cm, Clock: RealClock{}, Outside: outside}
	started := make(chan struct{})
	go func() { <-started; cancel() }()
	res := Execute(d, fakeTest{run: func(ctx RunCtx, _ *writer.Writer) (result.Outcome, error) {
		close(started)   // the claim and the acquire are already done on the live context
		<-ctx.Ctx.Done() // the outside cancel reaches the run
		return result.Outcome{}, ctx.Ctx.Err()
	}})
	if res.Verdict != result.Aborted {
		t.Fatalf("verdict = %q, want aborted", res.Verdict)
	}
	if v := cm.written("r1-result")["verdict"]; string(v) != `"aborted"` {
		t.Fatalf("the aborted run was not published under the outside cancel: stored verdict %s", v)
	}
}

// TestLostGateClosesHandle proves the acquired handle is closed even on a lost gate with no release, so renewal is
// joined and never runs after Execute returns (SiteLock.Close cancels and joins it).
func TestLostGateClosesHandle(t *testing.T) {
	seq := &sequence{}
	h := held(seq)
	d := deps(t, seq, h)
	res := Execute(d, fakeTest{run: func(RunCtx, *writer.Writer) (result.Outcome, error) {
		h.gate.Close() // the site lock is lost mid-run; the lost-gate branch skips release
		return result.Outcome{}, nil
	}})
	if res.Verdict != result.Aborted {
		t.Fatalf("verdict = %q, want aborted", res.Verdict)
	}
	if h.released != 0 {
		t.Errorf("a lost lock was released %d times, want none", h.released)
	}
	if h.closed != 1 {
		t.Errorf("the acquired handle was closed %d times, want once (its renewal must be joined)", h.closed)
	}
}

// TestDryRunMismatchRefused proves a command whose write intent and dryRun setting disagree is refused before the
// lock or writer: a writing test with dryRun missing cannot send real writes and still pass, and a non-writing test
// with dryRun set is refused too.
func TestDryRunMismatchRefused(t *testing.T) {
	rows := []struct {
		name   string
		writes *bool
		dryRun *bool
	}{
		{"writing test, dryRun missing", nil, nil},
		{"non-writing test, dryRun set", ptr(false), ptr(false)},
	}
	for _, r := range rows {
		t.Run(r.name, func(t *testing.T) {
			d := deps(t, nil, held(nil))
			d.Config.DryRun = r.dryRun
			res := Execute(d, fakeTest{run: passes, writes: r.writes})
			if res.Verdict != result.Refused {
				t.Fatalf("verdict = %q, want refused", res.Verdict)
			}
			if res.Lock.AcquiredAt != nil {
				t.Error("the lock was taken despite the dryRun mismatch")
			}
		})
	}
}

// TestResultJournalCounts proves result.json reports the journal's own counts (journal.Counts: attempts are RPCs that went out or may have),
// not unknown: after real writes, one of them unclear, and after a dry run, which has planned ops and no attempts.
func TestResultJournalCounts(t *testing.T) {
	rows := []struct {
		name   string
		dryRun bool
		run    func(RunCtx, *writer.Writer)
		want   [3]uint64 // ops, attempts, unclear
	}{
		{"writes", false, func(ctx RunCtx, w *writer.Writer) {
			write(ctx.Ctx, w, "k1", true, writer.Attempt{})
			write(ctx.Ctx, w, "k2", true, writer.Attempt{})
			write(ctx.Ctx, w, "k3", false, writer.Attempt{NoReply: true})
		}, [3]uint64{3, 3, 1}},
		{"dry run", true, func(ctx RunCtx, w *writer.Writer) {
			for _, k := range []string{"k1", "k2", "k3"} {
				_, _ = writer.Plan(ctx.Ctx, w, writer.Call{WriteCall: journal.WriteCall{Scope: "test", Kind: "k", Key: k, Idempotent: true}, Retry: writer.DefaultRetry})
			}
		}, [3]uint64{3, 0, 0}},
	}
	for _, r := range rows {
		t.Run(r.name, func(t *testing.T) {
			d := deps(t, nil, held(nil))
			d.Config.DryRun = ptr(r.dryRun)
			var j *journal.Journal
			d.NewJournal = func(dir string, h journal.Header) (*journal.Journal, error) {
				var err error
				j, err = journal.Create(dir, h)
				return j, err
			}
			Execute(d, fakeTest{run: func(ctx RunCtx, w *writer.Writer) (result.Outcome, error) {
				r.run(ctx, w)
				return result.OutcomeOf(result.Passed), nil
			}})
			data, err := os.ReadFile(result.ResultFor(d.Config.Journal.Dir, d.Config.RunID).Path)
			if err != nil {
				t.Fatal(err)
			}
			var got struct {
				Journal struct{ Ops, Attempts, Unclear *uint64 }
			}
			if err := json.Unmarshal(data, &got); err != nil || got.Journal.Ops == nil || got.Journal.Attempts == nil || got.Journal.Unclear == nil {
				t.Fatalf("result.json journal counts = %+v (%v), want all known", got.Journal, err)
			}
			c := j.Counts()
			if have := [3]uint64{*got.Journal.Ops, *got.Journal.Attempts, *got.Journal.Unclear}; have != r.want || have != [3]uint64{c.Ops, c.Attempts, c.Unclear} {
				t.Errorf("result.json counts = %v, journal's = %+v, want %v", have, c, r.want)
			}
		})
	}
}

// TestLostReplyAfterCommitKeepsRecordsInAgreement proves that when the final status write commits but its reply is
// lost, the ConfigMap and result.json carry the same verdict: the read-back confirms the write landed, so the clean
// verdict stays in both instead of result.json alone being downgraded.
func TestLostReplyAfterCommitKeepsRecordsInAgreement(t *testing.T) {
	seq := &sequence{}
	h := held(seq)
	d := deps(t, seq, h)
	cm := d.CM.(*cmFake)
	cm.loseReply = func(js string) bool { return strings.Contains(js, `"passed"`) } // the final clean write
	res := Execute(d, fakeTest{run: passes})
	if res.Verdict != result.Passed {
		t.Errorf("return verdict = %q, want passed (the write landed)", res.Verdict)
	}
	cmV := string(cm.written("r1-result")["verdict"])
	fileV := fileVerdict(d.Config.Journal.Dir, d.Config.RunID)
	if cmV != `"passed"` || fileV != "passed" {
		t.Fatalf("records disagree: ConfigMap=%s result.json=%q", cmV, fileV)
	}
}
