// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"

	"go.uber.org/goleak"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

func TestMain(m *testing.M) {
	goleak.VerifyTestMain(m)
}

func passes(RunCtx, *writer.Writer) (result.Outcome, error) {
	return result.OutcomeOf(result.Passed), nil
}

// TestPublishOrderPendingReleaseFinal proves the official record is never a success the release has not confirmed:
// a non-success pending record is written first, then the lock is released, then the final record.
func TestPublishOrderPendingReleaseFinal(t *testing.T) {
	seq := &sequence{}
	h := held(seq)
	d := deps(t, seq, h)
	res := Execute(d, fakeTest{run: passes})
	if res.Verdict != result.Passed {
		t.Errorf("verdict = %q, want passed", res.Verdict)
	}
	want := []string{"write:unresolved/pending", "release:final", "write:passed"}
	if got := seq.all(); !reflect.DeepEqual(got, want) {
		t.Fatalf("publish order = %v, want %v", got, want)
	}
	if res.Lock.FinalState == nil || *res.Lock.FinalState != "final" {
		t.Errorf("final state = %v, want final", res.Lock.FinalState)
	}
	final := d.CM.(*cmFake).written("r1-result")
	if string(final["verdict"]) != `"passed"` {
		t.Errorf("stored verdict = %s, want passed", final["verdict"])
	}
}

// TestStagesAndInvariantsReachTheResult: a test's stages and invariants reach the result, also when it returns an error with them.
func TestStagesAndInvariantsReachTheResult(t *testing.T) {
	seq := &sequence{}
	stages := []result.StageResult{{ID: "s", Status: result.StageUnresolved, Units: result.UnitCountsOf(1, 0, 0)}}
	invs := []result.InvariantResult{{Name: "i", Held: true}}
	res := Execute(deps(t, seq, held(seq)), fakeTest{run: func(RunCtx, *writer.Writer) (result.Outcome, error) {
		o := result.OutcomeOf(result.Unresolved)
		o.Stages, o.Invariants = stages, invs
		return o, errors.New("the sends ended")
	}})
	if res.Verdict != result.Aborted || !reflect.DeepEqual(res.Stages, stages) || !reflect.DeepEqual(res.Invariants, invs) {
		t.Errorf("result = %s, %+v, %+v; want aborted with the test's stages and invariants", res.Verdict, res.Stages, res.Invariants)
	}
}

// TestNotCleanWhenResultFileUnwritable proves a success whose authoritative result.json cannot be written is never
// published clean: the verdict is downgraded, the status reference is cleared and a reason says the file was not written.
func TestNotCleanWhenResultFileUnwritable(t *testing.T) {
	seq := &sequence{}
	d := deps(t, seq, held(seq))
	// Make the result file path a directory so the atomic rename over it fails.
	if err := os.Mkdir(filepath.Join(d.Config.Journal.Dir, "r1.result.json"), 0o755); err != nil {
		t.Fatal(err)
	}
	res := Execute(d, fakeTest{run: passes})
	if res.Verdict != result.Unresolved {
		t.Errorf("verdict = %q, want unresolved (downgraded from passed)", res.Verdict)
	}
	if res.Result.Claim != "" {
		t.Error("the result ref must be cleared when the file was not written")
	}
	if !slices.ContainsFunc(res.Reasons, func(r string) bool { return strings.Contains(r, "result file not written") }) {
		t.Errorf("a reason must say the result file was not written: %v", res.Reasons)
	}
	if v := d.CM.(*cmFake).written("r1-result")["verdict"]; string(v) != `"unresolved"` {
		t.Errorf("stored verdict = %s, want unresolved", v)
	}
}

// TestLostLockDuringRunAborts proves the held lock's gate is wired into the writer: closing it halts the next
// dispatch, and the run is aborted with no release of the lost lock.
func TestLostLockDuringRunAborts(t *testing.T) {
	seq := &sequence{}
	h := held(seq)
	d := deps(t, seq, h)
	var sendErr error
	res := Execute(d, fakeTest{run: func(ctx RunCtx, w *writer.Writer) (result.Outcome, error) {
		h.gate.Close() // the site lock is lost mid-run
		_, sendErr = writer.Send(ctx.Ctx, w, writer.Call{WriteCall: journal.WriteCall{Scope: "test", Kind: "k", Key: "key1"}, Retry: writer.DefaultRetry},
			func(context.Context) writer.Attempt { return writer.Attempt{} })
		return result.Outcome{}, sendErr
	}})
	if sendErr != writer.ErrLockLost {
		t.Errorf("the writer did not halt on the lost lock: %v", sendErr)
	}
	if res.Verdict != result.Aborted {
		t.Errorf("verdict = %q, want aborted", res.Verdict)
	}
	if res.Lock.LostAt == nil {
		t.Error("lostAt must be set when the lock is lost")
	}
	if h.released != 0 || res.Lock.FinalState != nil {
		t.Error("a lost lock must not be released")
	}
}

// TestRefusedClaimDoesNotOverwrite proves a duplicate attempt never writes over the owner's result.
func TestRefusedClaimDoesNotOverwrite(t *testing.T) {
	cm := newCM()
	cm.items["r1-result"] = cluster.ConfigMapObject{Name: "r1-result", ResourceVersion: "5", UID: "u-other",
		Annotations: map[string]string{result.OwnerAnnotation: "other/tok"}, Data: map[string]string{result.ResultKey: `{"verdict":"passed"}`}}
	d := deps(t, nil, held(nil))
	d.CM = cm
	res := Execute(d, fakeTest{run: passes})
	if res.Verdict != result.Refused {
		t.Errorf("verdict = %q, want refused", res.Verdict)
	}
	if v := cm.written("r1-result")["verdict"]; string(v) != `"passed"` {
		t.Errorf("the owner's result was overwritten: verdict now %s", v)
	}
	if !slices.ContainsFunc(res.Reasons, func(r string) bool { return strings.Contains(r, "owned by another attempt") }) {
		t.Errorf("a reason must explain the refused claim: %v", res.Reasons)
	}
}

// TestFoundReasonsAndDetailReachTheResult proves a test's reasons and findings land in the RunResult and its
// result.json, while the ConfigMap status keeps only the verdict and reasons.
func TestFoundReasonsAndDetailReachTheResult(t *testing.T) {
	d := deps(t, nil, held(nil))
	detail := readiness.Report{Checks: []readiness.Check{{Name: "probe", Detail: "probe: down"}, {Name: "controlPlanePods", OK: true}},
		Restarts: map[string]uint32{"nico/api-0": 3, "nico/api-1": 0}}.Detail()
	res := Execute(d, fakeTest{run: func(RunCtx, *writer.Writer) (result.Outcome, error) {
		return result.Found(result.NotReady, []string{"probe: down"}, detail), nil
	}})
	if res.Verdict != result.NotReady || res.ExitCode != result.ExitNotRun || !slices.Contains(res.Reasons, "probe: down") {
		t.Errorf("result = %q/%d %v, want notReady/2 with the test's reason", res.Verdict, res.ExitCode, res.Reasons)
	}
	if string(res.Detail) != string(detail) {
		t.Errorf("detail = %s, want %s", res.Detail, detail)
	}
	file, err := os.ReadFile(res.Result.Path)
	var got result.RunResult
	if err == nil {
		err = jsonx.Decode(file, &got)
	}
	if err != nil || got.Verdict != result.NotReady || string(got.Detail) != string(detail) {
		t.Errorf("result.json did not round-trip the detail: %v %s", err, got.Detail)
	}
	status := d.CM.(*cmFake).written("r1-result")
	if _, has := status["detail"]; has || !strings.Contains(string(status["reasons"]), "probe: down") {
		t.Errorf("status = %v, want the reasons and no detail", status)
	}
}

func TestAcceptedUnfinishedWorkRetainsLease(t *testing.T) {
	for _, settled := range []bool{false, true} {
		t.Run(fmt.Sprint(settled), func(t *testing.T) {
			seq := &sequence{}
			d := deps(t, seq, held(seq))
			res := Execute(d, fakeTest{run: func(rc RunCtx, w *writer.Writer) (result.Outcome, error) {
				if err := w.Pending(rc.Ctx, true); err != nil {
					return result.Outcome{}, err
				}
				_, err := writer.Send(rc.Ctx, w, writer.Call{WriteCall: journal.WriteCall{Key: "create", Kind: "instance"}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: time.Second}}, func(context.Context) writer.Attempt {
					return writer.Attempt{IDs: []string{"00000000-0000-4000-8000-000000000001"}}
				})
				if err == nil && settled {
					err = w.Pending(rc.Ctx, false)
				}
				return result.OutcomeOf(result.Passed), err
			}})
			want, state := result.Unresolved, "needsCleanup"
			if settled {
				want, state = result.Passed, "final"
			}
			if res.Verdict != want || res.Lock.FinalState == nil || *res.Lock.FinalState != state {
				t.Fatalf("verdict=%s lease=%v", res.Verdict, res.Lock.FinalState)
			}
		})
	}
}
