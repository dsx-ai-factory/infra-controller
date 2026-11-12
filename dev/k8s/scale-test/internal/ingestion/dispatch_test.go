// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"os"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

var _ ForgeWriter = (*forge.Client)(nil)

// secret is server text and a credential that must never reach an outcome, an error or a detail.
const secret = "SECRET factory_password https://user:pw@nico"

type reply struct {
	md  metadata.MD
	err error
}

func st(c codes.Code, hints ...string) reply {
	md := metadata.MD{}
	for _, h := range hints {
		md.Append("grpc-retry-pushback-ms", h)
	}
	return reply{md, status.Error(c, secret)}
}

// fakeForge records each call as "<kind> <id>", runs during inside it, then answers with reply (nil accepts) and sets the call's trailer.
type fakeForge struct {
	mu     sync.Mutex
	calls  []string
	reply  func(call string, n int) reply
	during func(ctx context.Context, call string)
}

func (f *fakeForge) do(ctx context.Context, kind, id string, opts []grpc.CallOption) error {
	call := kind + " " + id
	f.mu.Lock()
	f.calls = append(f.calls, call)
	n := len(slices.DeleteFunc(slices.Clone(f.calls), func(c string) bool { return c != call }))
	f.mu.Unlock()
	if f.during != nil {
		f.during(ctx, call)
	}
	if f.reply == nil {
		return nil
	}
	r := f.reply(call, n)
	for _, o := range opts {
		if tr, ok := o.(grpc.TrailerCallOption); ok {
			*tr.TrailerAddr = r.md
		}
	}
	return r.err
}

func (f *fakeForge) seen() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.calls)
}

func (f *fakeForge) AddExpectedRackGroup(ctx context.Context, b *corev1.ExpectedRackGroup, o ...grpc.CallOption) error {
	return f.do(ctx, "group", b.GetRackGroupId().GetId(), o)
}
func (f *fakeForge) AddExpectedRack(ctx context.Context, b *corev1.ExpectedRack, o ...grpc.CallOption) error {
	return f.do(ctx, "rack", b.GetRackId().GetId(), o)
}
func (f *fakeForge) AddExpectedMachine(ctx context.Context, b *corev1.ExpectedMachine, o ...grpc.CallOption) error {
	return f.do(ctx, "machine", b.GetBmcMacAddress(), o)
}
func (f *fakeForge) AddExpectedSwitch(ctx context.Context, b *corev1.ExpectedSwitch, o ...grpc.CallOption) error {
	return f.do(ctx, "switch", b.GetBmcMacAddress(), o)
}
func (f *fakeForge) AddExpectedPowerShelf(ctx context.Context, b *corev1.ExpectedPowerShelf, o ...grpc.CallOption) error {
	return f.do(ctx, "shelf", b.GetBmcMacAddress(), o)
}

type gate struct{ lost atomic.Bool }

func (g *gate) Check() error {
	if g.lost.Load() {
		return errors.New("lost")
	}
	return nil
}

type noStop struct{}

func (noStop) Stopped() <-chan struct{} { return nil }

type rig struct {
	j    *journal.Journal
	gate *gate
	w    *writer.Writer
	f    *fakeForge
	end  time.Time // the dispatch deadline, none when zero
}

func newRig(t *testing.T) *rig {
	j, err := journal.Create(t.TempDir(), journal.Header{RunID: "r1", Command: "ingestion"})
	if err != nil {
		t.Fatal(err)
	}
	r := &rig{j: j, gate: &gate{}, f: &fakeForge{}}
	r.w = writer.New(j, r.gate, noStop{}, time.Time{})
	t.Cleanup(func() { r.w.Close(); _ = j.Close() })
	return r
}

// dispatch plans inv and dispatches it, then joins every attempt.
func (r *rig) dispatch(t *testing.T, ctx context.Context, inv *inventory.SiteInventory, conc Concurrency, dryRun bool) Results {
	t.Helper()
	plan, recs, err := Records(inv)
	if err != nil {
		t.Fatal(err)
	}
	res := Dispatch(ctx, r.w, r.f, plan, recs, conc, r.end, dryRun)
	r.w.Close()
	return res
}

// statuses is each key's status, with the reason of a rejection.
func statuses(res Results) map[WriteKey]string {
	out := map[WriteKey]string{}
	for k, o := range res.Outcomes {
		out[k] = strings.TrimSuffix(string(o.Status)+" "+o.Reason, " ")
	}
	return out
}

var one = Concurrency{1, 1, 1, 1}

// TestRetryClassification drives one host write through the real writer per scripted reply sequence: what is a lost reply (retried) and what a rejection, the
// AlreadyExists accounting, that an unclear op leaves the journal unsettled, so the run releases the site needsCleanup, and the 30-attempt budget within the
// run deadline.
func TestRetryClassification(t *testing.T) {
	ok, pushback := reply{}, st(codes.ResourceExhausted, "500")
	for _, tc := range []struct {
		name      string
		script    []reply // its last reply repeats
		want      string
		attempts  uint32
		unsettled bool
		deadline  time.Duration // the run's, when set
	}{
		{"accepted", []reply{ok}, "sent", 1, false, 0},
		{"unavailable_is_a_lost_reply_retried", []reply{st(codes.Unavailable), ok}, "sent", 2, false, 0},
		{"an_error_with_no_status_is_a_lost_reply", []reply{{err: errors.New(secret)}, ok}, "sent", 2, false, 0},
		{"not_found_is_a_lost_reply", []reply{st(codes.NotFound), ok}, "sent", 2, false, 0},
		{"internal_is_a_lost_reply", []reply{st(codes.Internal), ok}, "sent", 2, false, 0},
		{"resource_exhausted_without_a_hint_is_a_lost_reply", []reply{st(codes.ResourceExhausted), ok}, "sent", 2, false, 0},
		{"resource_exhausted_with_a_hint_is_pushback_retried", []reply{pushback, ok}, "sent", 2, false, 0},
		{"invalid_argument_fails_unretried", []reply{st(codes.InvalidArgument)}, "rejected grpc-InvalidArgument", 1, false, 0},
		{"permission_denied_fails_unretried", []reply{st(codes.PermissionDenied)}, "rejected grpc-PermissionDenied", 1, false, 0},
		{"unauthenticated_fails_unretried", []reply{st(codes.Unauthenticated)}, "rejected grpc-Unauthenticated", 1, false, 0},
		{"failed_precondition_fails_unretried", []reply{st(codes.FailedPrecondition)}, "rejected grpc-FailedPrecondition", 1, false, 0},
		{"already_exists_first_existed_before_run", []reply{st(codes.AlreadyExists)}, "existedBeforeRun", 1, false, 0},
		// The writer keeps any rejection after a lost reply unclear; the Rust runner counted AlreadyExists here as this run's write.
		{"already_exists_after_a_lost_reply_is_unclear", []reply{st(codes.Unavailable), st(codes.AlreadyExists)}, "unclear", 0, true, 0},
		{"failed_precondition_after_a_lost_reply_is_unclear", []reply{st(codes.Unavailable), st(codes.FailedPrecondition)}, "unclear", 0, true, 0},
		{"a_rejection_after_a_lost_reply_is_unclear", []reply{st(codes.DeadlineExceeded), st(codes.InvalidArgument)}, "unclear", 0, true, 0},
		// A stop hint forbids a retry but does not prove the write did not land: the writer keeps it unclear.
		{"resource_exhausted_with_a_negative_hint_is_unclear", []reply{st(codes.ResourceExhausted, "-1")}, "unclear", 0, true, 0},
		// An expected-record write has 30 attempts, not the writer's generic 5.
		{"lost_replies_to_the_last_attempt_are_unclear", slices.Repeat([]reply{st(codes.Unavailable)}, 30), "unclear", 0, true, 0},
		{"five_pushbacks_then_accepted_on_the_sixth_attempt", append(slices.Repeat([]reply{pushback}, 5), ok), "sent", 6, false, 0},
		{"pushback_on_all_30_attempts_fails", slices.Repeat([]reply{pushback}, 30), "rejected pushback-grpc-ResourceExhausted", 30, false, 0},
		// The waits are 5, 5, 5, 8 and 16 s: the 30 s wait after the sixth attempt would end past a 60 s run deadline.
		{"the_run_deadline_ends_the_retries_first", slices.Repeat([]reply{pushback}, 6), "rejected pushback-grpc-ResourceExhausted", 6, false, time.Minute},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRig(t)
				if tc.deadline > 0 {
					r.w = writer.New(r.j, r.gate, noStop{}, time.Now().Add(tc.deadline))
				}
				r.f.reply = func(_ string, n int) reply { return tc.script[min(n, len(tc.script))-1] }
				res := r.dispatch(t, context.Background(), inv(nil, inventory.Device{Kind: inventory.KindMachine, BmcMac: "aa", Serial: "m001"}), one, false)
				o := res.Outcomes["host/aa"]
				is(t, []any{statuses(res)["host/aa"], o.Attempts, len(r.f.seen()), r.j.Unsettled(), res.Err},
					[]any{tc.want, tc.attempts, len(tc.script), tc.unsettled, nil}, "status, attempts, calls, unsettled, error")
				if o.Status == Sent && (o.SentAt.IsZero() || o.RepliedAt.Before(o.SentAt)) {
					t.Errorf("sent at %v, replied at %v", o.SentAt, o.RepliedAt)
				}
				if s := fmt.Sprintf("%+v", res); strings.Contains(s, "SECRET") || strings.Contains(s, "factory_password") {
					t.Errorf("server text or a credential leaked: %s", s)
				}
			})
		})
	}
}

// TestAKnownOutcomeSurvivesAReceiptFailure: when the receipt of a write cannot be persisted the writer still returns the outcome it knows. The key keeps it with its
// attempts, timing or reason, the error stops every later write, and the journal stays unsettled. A lost reply stays unclear.
func TestAKnownOutcomeSurvivesAReceiptFailure(t *testing.T) {
	ok := reply{}
	for _, tc := range []struct {
		name     string
		script   []reply // the receipt of its last reply fails
		want     string
		attempts uint32
	}{
		{"accepted", []reply{ok}, "sent", 1},
		{"accepted_on_a_retry", []reply{st(codes.Unavailable), ok}, "sent", 2},
		{"invalid_argument", []reply{st(codes.InvalidArgument)}, "rejected grpc-InvalidArgument", 1},
		{"already_exists", []reply{st(codes.AlreadyExists)}, "existedBeforeRun", 1},
		{"a_rejection_after_a_lost_reply_stays_unclear", []reply{st(codes.Unavailable), st(codes.InvalidArgument)}, "unclear", 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRig(t)
				var calls int
				r.f.during = func(context.Context, string) {
					if calls++; calls == len(tc.script) {
						r.j.Inject(func(*os.File, []byte) error { return errors.New("injected: sync failed") })
					}
				}
				r.f.reply = func(_ string, n int) reply { return tc.script[min(n, len(tc.script))-1] }
				res := r.dispatch(t, context.Background(), fixture(), one, false)
				o := res.Outcomes["rackGroup/r1"]
				is(t, []any{statuses(res)["rackGroup/r1"], o.Attempts, errors.Is(res.Err, journal.ErrIO), len(r.f.seen()), r.j.Unsettled()},
					[]any{tc.want, tc.attempts, true, len(tc.script), true}, "status, attempts, io error, calls, unsettled")
				if tc.want == "sent" && (o.SentAt.IsZero() || o.RepliedAt.Before(o.SentAt)) {
					t.Errorf("sent at %v, replied at %v", o.SentAt, o.RepliedAt)
				}
				for k, o := range res.Outcomes {
					if k != "rackGroup/r1" && o.Status != NotSent {
						t.Errorf("%s: %s after the stop", k, o.Status)
					}
				}
			})
		})
	}
}

// TestAttemptDetailIsLocalText: the detail the writer logs names the code at most, never the server's message or an error's text; a body of no write kind is
// never accepted.
func TestAttemptDetailIsLocalText(t *testing.T) {
	for _, err := range []error{errors.New(secret), status.Error(codes.InvalidArgument, secret), status.Error(codes.Unavailable, secret),
		status.Error(codes.ResourceExhausted, secret), status.Error(codes.AlreadyExists, secret)} {
		f := &fakeForge{reply: func(string, int) reply { return reply{err: err} }}
		a := attempt(context.Background(), f, &corev1.ExpectedMachine{BmcMacAddress: "aa", BmcPassword: "factory_password"})
		if strings.Contains(a.Detail, "SECRET") || strings.Contains(a.Detail, "factory_password") {
			t.Errorf("%v: detail %q", status.Code(err), a.Detail)
		}
	}
	if a := attempt(context.Background(), &fakeForge{}, &corev1.ExpectedRackGroupMember{}); a.Rejected == nil {
		t.Errorf("a body of no write kind counted as accepted: %+v", a)
	}
}

func TestDispatchOrder(t *testing.T) {
	r := newRig(t)
	r.f.reply = func(call string, _ int) reply {
		if call == "group g-r2" {
			return st(codes.InvalidArgument)
		}
		return reply{}
	}
	res := r.dispatch(t, context.Background(), fixture(), DefaultConcurrency, false)
	is(t, statuses(res), map[WriteKey]string{
		"rackGroup/r1": "sent", "rack/r1": "sent",
		"rackGroup/r2": "rejected grpc-InvalidArgument", "rack/r2": "groupNotAccepted",
		"host/aa:00": "sent", "powerShelf/cc:00": "sent", // on r1
		"host/aa:01":   "sent", // names no rack
		"switch/bb:00": "rackNotAccepted",
	}, "outcomes")
	is(t, res.Err, nil, "no error")
	calls := r.f.seen()
	at := func(c string) int { return slices.Index(calls, c) }
	if at("group g-r1") > at("rack r1") || at("rack r2") != -1 || at("switch bb:00") != -1 {
		t.Errorf("a rack went before its group or past a rejected one: %v", calls)
	}
	if racks := slices.IndexFunc(calls, func(c string) bool { return !strings.HasPrefix(c, "group ") && !strings.HasPrefix(c, "rack ") }); racks < 3 ||
		slices.ContainsFunc(calls[racks:], func(c string) bool { return strings.HasPrefix(c, "group ") || strings.HasPrefix(c, "rack ") }) {
		t.Errorf("a device went before the racks finished: %v", calls)
	}
}

// TestAlreadyExistsBlocksDescendants: a group or rack that already exists is not this run's, so what waits on it is not sent, the journal settles, and the
// other branches go on; a lost reply then AlreadyExists stays unclear and unsettled.
func TestAlreadyExistsBlocksDescendants(t *testing.T) {
	ok := reply{}
	for _, tc := range []struct {
		name      string
		call      string
		script    []reply
		want      map[WriteKey]string
		calls     []string
		unsettled bool
	}{
		{"group", "group g-r2", []reply{st(codes.AlreadyExists)}, map[WriteKey]string{
			"rackGroup/r1": "sent", "rack/r1": "sent", "rackGroup/r2": "existedBeforeRun", "rack/r2": "groupNotAccepted",
			"host/aa:00": "sent", "powerShelf/cc:00": "sent", "host/aa:01": "sent", "switch/bb:00": "rackNotAccepted"},
			[]string{"group g-r1", "group g-r2", "machine AA:00", "machine aa:01", "rack r1", "shelf cc:00"}, false},
		{"rack", "rack r1", []reply{st(codes.AlreadyExists)}, map[WriteKey]string{
			"rackGroup/r1": "sent", "rack/r1": "existedBeforeRun", "rackGroup/r2": "sent", "rack/r2": "sent",
			"host/aa:00": "rackNotAccepted", "powerShelf/cc:00": "rackNotAccepted", "host/aa:01": "sent", "switch/bb:00": "sent"},
			[]string{"group g-r1", "group g-r2", "machine aa:01", "rack r1", "rack r2", "switch bb:00"}, false},
		{"a lost reply then AlreadyExists", "group g-r2", []reply{st(codes.Unavailable), st(codes.AlreadyExists)}, map[WriteKey]string{
			"rackGroup/r1": "sent", "rack/r1": "sent", "rackGroup/r2": "unclear", "rack/r2": "groupNotAccepted",
			"host/aa:00": "sent", "powerShelf/cc:00": "sent", "host/aa:01": "sent", "switch/bb:00": "rackNotAccepted"},
			[]string{"group g-r1", "group g-r2", "group g-r2", "machine AA:00", "machine aa:01", "rack r1", "shelf cc:00"}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRig(t)
				r.f.reply = func(call string, n int) reply {
					if call == tc.call {
						return tc.script[min(n, len(tc.script))-1]
					}
					return ok
				}
				res := r.dispatch(t, context.Background(), fixture(), one, false)
				is(t, []any{statuses(res), slices.Sorted(slices.Values(r.f.seen())), r.j.Unsettled(), res.Err}, []any{tc.want, tc.calls, tc.unsettled, nil},
					"statuses, calls, unsettled, error")
			})
		})
	}
}

func TestStreamsRunInParallelWithinTheirConcurrency(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		var mu sync.Mutex
		inflight, peak := map[string]int{}, map[string]int{}
		r.f.during = func(_ context.Context, call string) {
			kind := strings.Fields(call)[0]
			mu.Lock()
			inflight[kind]++
			inflight["all"]++
			peak[kind], peak["all"] = max(peak[kind], inflight[kind]), max(peak["all"], inflight["all"])
			mu.Unlock()
			time.Sleep(time.Second)
			mu.Lock()
			inflight[kind]--
			inflight["all"]--
			mu.Unlock()
		}
		var devices []inventory.Device
		for i, kind := range []inventory.DeviceKind{"machine", "machine", "machine", "machine", "switch", "switch", "power_shelf", "power_shelf"} {
			devices = append(devices, inventory.Device{Kind: kind, BmcMac: fmt.Sprint(i), Serial: "m00" + fmt.Sprint(i)})
		}
		start := time.Now()
		res := r.dispatch(t, context.Background(), inv(nil, devices...), Concurrency{Rack: 1, Host: 2, Switch: 1, PowerShelf: 1}, false)
		is(t, peak, map[string]int{"machine": 2, "switch": 1, "shelf": 1, "all": 4}, "peak in flight")
		is(t, time.Since(start), 2*time.Second, "the streams overlap")
		for k, o := range res.Outcomes {
			is(t, o.Status, Sent, string(k))
		}
	})
}

// TestAStopLeavesTheRestNotSent: a lock loss or a cancel during the first write stops every later send; the write in flight finishes, accepted or unclear.
func TestAStopLeavesTheRestNotSent(t *testing.T) {
	for _, tc := range []struct {
		name    string
		stop    func(r *rig, cancel context.CancelFunc)
		want    error
		first   string
		settled bool
	}{
		{"lock lost", func(r *rig, _ context.CancelFunc) { r.gate.lost.Store(true) }, writer.ErrLockLost, "sent", true},
		{"cancel", func(_ *rig, cancel context.CancelFunc) { cancel() }, writer.ErrSoftStop, "unclear", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := newRig(t)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			r.f.during = func(context.Context, string) { tc.stop(r, cancel) }
			res := r.dispatch(t, ctx, fixture(), one, false)
			want := map[WriteKey]string{}
			for k := range res.Outcomes {
				want[k] = "notSent"
			}
			want["rackGroup/r1"] = tc.first
			is(t, statuses(res), want, "outcomes")
			is(t, []any{errors.Is(res.Err, tc.want), r.f.seen(), !r.j.Unsettled()}, []any{true, []string{"group g-r1"}, tc.settled}, "error, calls, settled")
		})
	}
}

// TestDryRunJournalsAndSendsNothing: a dry run journals every key and sends none, each key planned with no attempt, send, reply or dispatch window, a planned
// receipt and log line and never an accepted one; a run sends every key and logs it accepted.
func TestDryRunJournalsAndSendsNothing(t *testing.T) {
	for _, dryRun := range []bool{true, false} {
		var logs bytes.Buffer
		old := slog.Default()
		slog.SetDefault(slog.New(slog.NewTextHandler(&logs, nil)))
		t.Cleanup(func() { slog.SetDefault(old) })
		r := newRig(t)
		res := r.dispatch(t, context.Background(), fixture(), one, dryRun)
		data, err := os.ReadFile(r.j.Path())
		if err != nil {
			t.Fatal(err)
		}
		want, calls, planned, acc := Sent, 8, 0, 8
		if dryRun {
			want, calls, planned, acc = Planned, 0, 8, 0
		}
		is(t, []any{len(r.f.seen()), strings.Count(string(data), `"entry":"intent"`), len(res.Outcomes), res.Err}, []any{calls, 8, 8, nil},
			"calls, intents journaled, outcomes, error")
		is(t, []any{strings.Count(string(data), `"kind":"planned"`), strings.Count(string(data), `"kind":"accepted"`), strings.Count(logs.String(), "write planned"),
			strings.Count(logs.String(), "write accepted"), len(res.Windows) == 0, r.j.Unsettled()},
			[]any{planned, acc, planned, acc, dryRun, false}, "planned and accepted receipts, planned and accepted logs, no windows, settled")
		for k, o := range res.Outcomes {
			is(t, []any{o.Status, o.Attempts == 0, o.SentAt.IsZero(), o.RepliedAt.IsZero()}, []any{want, dryRun, dryRun, dryRun}, string(k))
		}
	}
}

func TestDispatchRefusesBadInput(t *testing.T) {
	f := fixture()
	f.Pods[0].Records.Devices = append(f.Pods[0].Records.Devices, inventory.Device{Kind: inventory.KindMachine, BmcMac: secret, Serial: "m003"})
	plan, recs, err := Records(f)
	if err != nil {
		t.Fatal(err)
	}
	// with is recs with k's record replaced, or removed when r is nil.
	with := func(k WriteKey, r *Record) map[WriteKey]Record {
		m := maps.Clone(recs)
		delete(m, k)
		if r != nil {
			m[k] = *r
		}
		return m
	}
	noBody := "a planned write has no body of its kind (write 2)" // rack/r1, after the two groups
	for _, tc := range []struct {
		name string
		noW  bool
		f    ForgeWriter
		recs map[WriteKey]Record
		conc Concurrency
		want string
	}{
		{"a planned key with no body", false, &fakeForge{}, with("rack/r1", nil), one, noBody},
		{"a nil body", false, &fakeForge{}, with("rack/r1", &Record{Kind: "expectedRack", Body: (*corev1.ExpectedRack)(nil)}), one, noBody},
		{"a body of another kind", false, &fakeForge{}, with("rack/r1", &Record{Kind: "expectedRack", Body: recs["host/aa:00"].Body}), one, noBody},
		{"a key holding inventory text with no body", false, &fakeForge{}, with(DeviceKey(Host, secret), nil), one,
			"a planned write has no body of its kind (write 6)"},
		{"no records", false, &fakeForge{}, nil, one, "a planned write has no body of its kind (write 0)"},
		{"a zero concurrency", false, &fakeForge{}, recs, Concurrency{1, 1, 0, 1}, "every write concurrency must be at least 1"},
		{"no writer", true, &fakeForge{}, recs, one, "ingestion dispatch needs a writer and a nico-api client"},
		{"no forge", false, nil, recs, one, "ingestion dispatch needs a writer and a nico-api client"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := newRig(t)
			w := r.w
			if tc.noW {
				w = nil
			}
			res := Dispatch(context.Background(), w, tc.f, plan, tc.recs, tc.conc, time.Time{}, false)
			is(t, fmt.Sprint(res.Err), tc.want, "error")
			for k := range recs {
				is(t, res.Outcomes[k].Status, NotSent, string(k))
			}
			if f, ok := tc.f.(*fakeForge); ok && len(f.seen()) != 0 {
				t.Errorf("sent %v", f.seen())
			}
		})
	}
}

// TestWindowsOpenAtTheFirstUnitDispatch: a stream's T0 is its first unit dispatch, never its rack group's, set even when that send is rejected; its end is the
// last accepted reply, moved to the return of an unclear send, which may have landed by then. A stream never dispatched has no window.
func TestWindowsOpenAtTheFirstUnitDispatch(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r, start := newRig(t), time.Now()
		r.f.during = func(context.Context, string) { time.Sleep(time.Second) }
		r.f.reply = func(call string, _ int) reply {
			switch call {
			case "rack r1":
				return st(codes.InvalidArgument)
			case "machine aa:01":
				return st(codes.Unavailable)
			}
			return reply{}
		}
		res := r.dispatch(t, context.Background(), fixture(), one, false)
		got := map[Stream][2]time.Duration{}
		for s, w := range res.Windows {
			got[s] = [2]time.Duration{w.T0.Sub(start), w.End.Sub(start)}
		}
		is(t, got, map[Stream][2]time.Duration{Rack: {time.Second, 4 * time.Second}, Switch: {4 * time.Second, 5 * time.Second},
			Host: {4 * time.Second, time.Since(start)}}, "windows")
		is(t, res.Outcomes["host/aa:01"].Status, Unclear, "the host write")
	})
}

// TestT0IsTheFirstAttemptOut: a stream's T0 is its first attempt past the writer's last gate and stop check: none when the gate closes after the intent or
// under dryRun, the RPC time after a slow intent, and set by a rejected attempt, whose end stays unknown. An RPC out before the deadline is not cut by it: its late reply is accepted, ending the window, and the next key is notSent. No attempt goes out at or past the deadline: with an
// intent held across it, the send begun before it is notDispatched with no RPC or T0, and the next key is notSent, never journaled.
func TestT0IsTheFirstAttemptOut(t *testing.T) {
	hold := func(d time.Duration) func(*rig) {
		return func(r *rig) { r.j.Inject(func(*os.File, []byte) error { time.Sleep(d); return nil }) }
	}
	for _, tc := range []struct {
		name   string
		setup  func(r *rig)
		dryRun bool
		want   map[Stream][2]time.Duration // T0 and End after the start; -1 for none
		counts [4]int                      // intents, RPCs, notDispatched receipts, dispatched attempts
	}{
		{"gate closed after the intent", func(r *rig) { r.j.Inject(func(*os.File, []byte) error { r.gate.lost.Store(true); return nil }) }, false, map[Stream][2]time.Duration{}, [4]int{1, 0, 1, 0}},
		{"dry run", func(*rig) {}, true, map[Stream][2]time.Duration{}, [4]int{2, 0, 0, 0}},
		{"slow intent", hold(5 * time.Second), false, map[Stream][2]time.Duration{Host: {5 * time.Second, 5 * time.Second}}, [4]int{2, 2, 0, 2}},
		{"rejected", func(r *rig) { r.f.reply = func(string, int) reply { return st(codes.InvalidArgument) } }, false, map[Stream][2]time.Duration{Host: {0, -1}}, [4]int{2, 2, 0, 2}},
		{"answer after the deadline", func(r *rig) { r.f.during = func(context.Context, string) { time.Sleep(15 * time.Second) } }, false, map[Stream][2]time.Duration{Host: {0, 15 * time.Second}}, [4]int{1, 1, 0, 1}},
		{"intent held past the deadline", hold(15 * time.Second), false, map[Stream][2]time.Duration{}, [4]int{1, 0, 1, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r, start := newRig(t), time.Now()
				r.end = start.Add(10 * time.Second)
				tc.setup(r)
				res := r.dispatch(t, context.Background(), inv(nil, inventory.Device{Kind: "machine", BmcMac: "0", Serial: "m000"}, inventory.Device{Kind: "machine", BmcMac: "1", Serial: "m001"}), one, tc.dryRun)
				got := map[Stream][2]time.Duration{}
				for s, w := range res.Windows {
					if got[s] = [2]time.Duration{w.T0.Sub(start), w.End.Sub(start)}; w.End.IsZero() {
						got[s] = [2]time.Duration{w.T0.Sub(start), -1}
					}
				}
				data, _ := os.ReadFile(r.j.Path())
				is(t, []any{got, [4]int{strings.Count(string(data), `"entry":"intent"`), len(r.f.seen()), strings.Count(string(data), `"kind":"notDispatched"`), int(r.j.Counts().Attempts)}},
					[]any{tc.want, tc.counts}, "windows, intents, RPCs, notDispatched receipts and dispatched attempts")
			})
		})
	}
}

// TestCutoffKeepsDescendantsNotSent checks dispatch through reconciliation: a cutoff is not a rejected prerequisite, and no unit stream began.
func TestCutoffKeepsDescendantsNotSent(t *testing.T) {
	for _, tc := range []struct {
		name  string
		calls []string
	}{
		{"group intent held past cutoff", nil},
		{"group accepted past cutoff", []string{"group g-r1"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r, inv := newRig(t), site()
				r.end = time.Now().Add(10 * time.Second)
				if len(tc.calls) == 0 {
					r.j.Inject(func(*os.File, []byte) error { time.Sleep(15 * time.Second); return nil })
				} else {
					r.f.during = func(context.Context, string) { time.Sleep(15 * time.Second) }
				}
				res := r.dispatch(t, context.Background(), inv, one, false)
				plan, _, err := Records(inv)
				if err != nil {
					t.Fatal(err)
				}
				for _, k := range plan.keys() {
					want := NotSent
					if len(tc.calls) > 0 && k == GroupKey("r1") {
						want = Sent
					}
					is(t, res.Outcomes[k].Status, want, string(k))
				}
				data, _ := os.ReadFile(r.j.Path())
				n := len(tc.calls)
				is(t, []any{res.Err, r.f.seen(), len(res.Windows), r.j.Unsettled(), [5]int{strings.Count(string(data), `"entry":"intent"`), len(r.f.seen()),
					strings.Count(string(data), `"kind":"notDispatched"`), strings.Count(string(data), `"kind":"accepted"`), int(r.j.Counts().Attempts)}},
					[]any{nil, tc.calls, 0, false, [5]int{1, n, 1 - n, n, n}}, "error, calls, windows, unsettled, intents, RPCs, receipts and attempts")
				rec := reconcile(plan, inv, res.Outcomes, endOf(nil))
				var stages []result.StageResult
				for _, st := range Stages(inv.N) {
					is(t, *rec.Tallies[st.ID], Tally{Unknown: st.N, Unproven: st.N}, string(st.ID))
					stages = append(stages, stageResult(st, rec.Tallies[st.ID], result.StageUnresolved))
				}
				is(t, result.Evaluate(result.Admitted{Stages: stages, Planned: len(stages)}, nil), result.Unresolved, "unsent units do not fail the run")
			})
		})
	}
}
