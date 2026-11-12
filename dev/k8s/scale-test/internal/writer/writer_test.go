// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package writer

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
)

// fakeGate is the site lock's write gate; Close loses it.
type fakeGate struct{ lost atomic.Bool }

func (g *fakeGate) Check() error {
	if g.lost.Load() {
		return errors.New("lost")
	}
	return nil
}
func (g *fakeGate) Close() { g.lost.Store(true) }

type stopper struct {
	ch   chan struct{}
	once sync.Once
}

func newStopper() *stopper                  { return &stopper{ch: make(chan struct{})} }
func (s *stopper) Stopped() <-chan struct{} { return s.ch }
func (s *stopper) stop()                    { s.once.Do(func() { close(s.ch) }) }

type rig struct {
	dir       string
	j         *journal.Journal
	gate      *fakeGate
	stop      *stopper
	ctx       context.Context
	cancel    context.CancelFunc
	w         *Writer
	onAttempt func(context.Context)
	calls     []int32 // attempts each send made, filled by runSends
}

func newRig(t *testing.T) *rig {
	r := &rig{dir: t.TempDir(), gate: &fakeGate{}, stop: newStopper()}
	j, err := journal.Create(r.dir, journal.Header{RunID: "r1", Command: "burst"})
	if err != nil {
		t.Fatal(err)
	}
	r.j = j
	r.ctx, r.cancel = context.WithCancel(context.Background())
	r.w = New(j, r.gate, r.stop, time.Time{})
	t.Cleanup(func() { r.cancel(); r.w.Close(); _ = j.Close() })
	return r
}

func acc() Attempt     { return Attempt{} }
func noReply() Attempt { return Attempt{NoReply: true, Detail: "connection reset"} }

// pending never answers: it waits for its context.
var pending = Attempt{Detail: "pending"}

func sec(n float64) *time.Duration { d := time.Duration(n * float64(time.Second)); return &d }

func rejn(reason journal.Reason, retryable bool, after *time.Duration) Attempt {
	return Attempt{Rejected: &Rejection{reason, retryable, after}, Detail: "status"}
}

// grpcRej is a rejection as the gRPC transport classifies a status with zero,
// one or several grpc-retry-pushback-ms trailer values.
func grpcRej(c codes.Code, hints ...string) Attempt {
	md := metadata.MD{}
	for _, h := range hints {
		md.Append("grpc-retry-pushback-ms", h)
	}
	r := ClassifyGRPC(c, md)
	return Attempt{Rejected: &r, Detail: "grpc"}
}

func times(n int, a Attempt) []Attempt { return slices.Repeat([]Attempt{a}, n) }

func cmpOr(s, fallback string) string {
	if s == "" {
		return fallback
	}
	return s
}

func mkCall(key, digest string, tweak func(*Call)) Call {
	c := Call{WriteCall: journal.WriteCall{Scope: "test", Kind: "batch", Key: key, Idempotent: true,
		Hosts: []string{"fm100h1"}, RequestDigest: digest}, Retry: DefaultRetry}
	if tweak != nil {
		tweak(&c)
	}
	return c
}

func nonIdem(c *Call) { c.Idempotent = false }

type sendSpec struct {
	key, digest string // k1, d1 when empty
	tweak       func(*Call)
	script      []Attempt
	want        string
	calls       int32 // expected dispatched attempts (checked unconditionally)
}

func (r *rig) send(t *testing.T, s sendSpec) (string, int32) {
	var n atomic.Int32
	out, err := Send(r.ctx, r.w, mkCall(cmpOr(s.key, "k1"), cmpOr(s.digest, "d1"), s.tweak), func(ctx context.Context) Attempt {
		i := int(n.Add(1)) - 1
		if i >= len(s.script) {
			return Attempt{NoReply: true, Detail: "an attempt past the script"}
		}
		a := s.script[i]
		if r.onAttempt != nil {
			r.onAttempt(ctx)
		}
		if a.Detail == "pending" {
			<-ctx.Done()
		}
		return a
	})
	r.w.Close() // every attempt called has returned
	return show(out, err), n.Load()
}

var named = map[error]string{ErrLockLost: "lockLost", ErrSoftStop: "softStop", ErrUnclearKey: "unclearKey",
	ErrKeyBusy: "keyBusy", journal.ErrIO: "io", errDeadline: "deadline"}

func show(o Outcome, err error) string {
	if err != nil {
		s := err.Error()
		for e, name := range named {
			if errors.Is(err, e) {
				s = name
			}
		}
		if oe := (*OpError)(nil); errors.As(err, &oe) {
			s = fmt.Sprintf("op%d dispatched=%t %s", oe.Op, oe.Dispatched, s)
		}
		return s
	}
	switch o.Kind {
	case Accepted:
		return fmt.Sprintf("accepted x%d", o.Attempts)
	case Failed:
		return fmt.Sprintf("failed %s x%d", o.Reason, o.Attempts)
	case Planned:
		return fmt.Sprintf("planned op%d", o.Op)
	}
	return fmt.Sprintf("unclear op%d", o.Op)
}

func rawLines(t *testing.T, j *journal.Journal) []map[string]any {
	t.Helper()
	data, err := os.ReadFile(j.Path())
	if err != nil {
		t.Fatal(err)
	}
	var out []map[string]any
	// Only newline-terminated lines are durable; the segment after the last
	// "\n" is the unterminated tail a crash recovery read skips (empty for a
	// clean file, a torn line otherwise).
	parts := strings.Split(string(data), "\n")
	for _, ln := range parts[:len(parts)-1] {
		if ln == "" {
			continue
		}
		var m map[string]any
		if err := json.Unmarshal([]byte(ln), &m); err != nil {
			t.Fatal(err) // an interleaved line
		}
		out = append(out, m)
	}
	return out
}

// states rebuilds each op from the raw journal, as a crash recovery read
// would: an accepted attempt settles it, else a dispatched attempt without a
// receipt leaves it unclear, else the last rejection fails it.
func (r *rig) states(t *testing.T) string {
	type op struct {
		key               string
		attempts          int
		accepted, unclear bool
		reason            journal.Reason
	}
	ops := map[float64]*op{}
	var order []float64
	receipt := map[float64]map[string]any{} // seq -> outcome
	for _, m := range rawLines(t, r.j) {
		if m["entry"] == "receipt" {
			receipt[m["seq"].(float64)] = m["outcome"].(map[string]any)
		}
	}
	for _, m := range rawLines(t, r.j) {
		if m["entry"] != "intent" {
			continue
		}
		n := m["op"].(float64)
		o := ops[n]
		if o == nil {
			o = &op{key: m["key"].(string)}
			ops[n], order = o, append(order, n)
		}
		o.attempts++
		out, ok := receipt[m["seq"].(float64)]
		switch {
		case !ok:
			o.unclear = true
		case out["kind"] == journal.Accepted:
			o.accepted = true
		case out["kind"] == journal.Rejected:
			o.reason = journal.Reason(out["reason"].(string))
		}
	}
	slices.Sort(order)
	var s []string
	for _, n := range order {
		o := ops[n]
		x := o.key + "="
		switch {
		case o.accepted:
			x += "settled"
		case o.unclear:
			x += "unclear"
		case o.reason != "":
			x += "failed(" + string(o.reason) + ")"
		default:
			x += "notDispatched"
		}
		if o.attempts > 1 {
			x += fmt.Sprintf("x%d", o.attempts)
		}
		s = append(s, x)
	}
	return strings.Join(s, " ")
}

// failSync fails the receipt append before its bytes land: a crash recovery
// read never sees the receipt, so the op reads as unclear, not settled.
func failSync(*os.File, []byte) error {
	return errors.New("injected: sync failed")
}

type sendRow struct {
	name   string
	setup  func(*testing.T, *rig)
	sends  []sendSpec
	states string
	took   string // "" when not checked
	check  func(*testing.T, *rig)
}

func TestSend(t *testing.T) {
	closeGate := func(r *rig) { r.gate.Close() }
	stop := func(r *rig) { r.stop.stop() }
	onIntent := func(attempt float64, f func(*rig)) func(*testing.T, *rig) {
		return func(_ *testing.T, r *rig) {
			r.j.AfterSync = func(e any) {
				if in, ok := e.(journal.Intent); ok && float64(in.Attempt) == attempt {
					f(r)
				}
			}
		}
	}
	// A retry's intent sync that outlasts the 6 s cutoff, whichever wait led to it.
	stallRetry := onIntent(2, func(*rig) { time.Sleep(10 * time.Second) })
	cutoffAt6s := func(c *Call) { c.Cutoff = time.Now().Add(6 * time.Second) }
	// afterStall: one notDispatched receipt (the retry) and the journal's counts and uncertainty.
	afterStall := func(counts journal.Counts, unsettled bool) func(*testing.T, *rig) {
		return func(t *testing.T, r *rig) {
			n := 0
			for _, m := range rawLines(t, r.j) {
				if m["entry"] == "receipt" && m["outcome"].(map[string]any)["kind"] == journal.NotDispatched {
					n++
				}
			}
			if r.j.Counts() != counts || r.j.Unsettled() != unsettled || n != 1 {
				t.Fatalf("counts %+v unsettled %t notDispatched %d", r.j.Counts(), r.j.Unsettled(), n)
			}
		}
	}
	var order []string
	rows := []sendRow{
		{name: "journal_intent_is_synced_before_call", setup: func(_ *testing.T, r *rig) {
			order = nil
			r.j.AfterSync = func(e any) { order = append(order, fmt.Sprintf("%T", e)) }
			r.onAttempt = func(context.Context) { order = append(order, "dispatch") }
		}, sends: []sendSpec{{script: []Attempt{acc()}, want: "accepted x1", calls: 1}}, states: "k1=settled",
			check: func(t *testing.T, _ *rig) {
				if got := strings.Join(order, " "); got != "journal.Intent dispatch journal.Receipt" {
					t.Fatal(got)
				}
			}},
		{name: "lock_lost_during_intent_sync_blocks_dispatch", setup: onIntent(1, closeGate),
			sends: []sendSpec{{script: []Attempt{acc()}, want: "op1 dispatched=false lockLost", calls: 0}}, states: "k1=notDispatched"},
		{name: "soft_stop_during_intent_sync_blocks_dispatch", setup: onIntent(1, stop),
			sends: []sendSpec{{script: []Attempt{acc()}, want: "op1 dispatched=false softStop", calls: 0}}, states: "k1=notDispatched"},
		{name: "lock_lost_during_a_retry_intent_blocks_the_retry", setup: onIntent(2, closeGate),
			sends:  []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable), acc()}, want: "op1 dispatched=true lockLost", calls: 1}},
			states: "k1=unclearx2"}, // attempt 1 lost its reply, the retry is not dispatched
		{name: "write_refused_when_gate_closed", setup: func(_ *testing.T, r *rig) { closeGate(r) },
			sends: []sendSpec{{script: []Attempt{acc()}, want: "lockLost", calls: 0}}},
		{name: "idempotent_retry_after_no_reply_settles_op",
			sends: []sendSpec{{script: []Attempt{noReply(), acc()}, want: "accepted x2", calls: 2}}, states: "k1=settledx2"},
		{name: "non_idempotent_unclear_never_resent", sends: []sendSpec{
			{tweak: nonIdem, script: []Attempt{noReply(), acc()}, want: "unclear op1", calls: 1},
			{tweak: nonIdem, script: []Attempt{acc()}, want: "keyBusy", calls: 0}, // the used key refuses a second op
		}, states: "k1=unclear"},
		{name: "a_rejection_that_may_follow_the_write_leaves_it_unclear", sends: []sendSpec{
			{script: times(5, grpcRej(codes.Internal)), want: "unclear op1", calls: 5},
			{key: "k2", tweak: nonIdem, script: []Attempt{grpcRej(codes.Canceled), acc()}, want: "unclear op2", calls: 1},
		}, states: "k1=unclearx5 k2=unclear"},
		{name: "an_unavailable_stop_hint_is_unclear_and_not_resent",
			sends:  []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable, "-1"), acc()}, want: "unclear op1", calls: 1}},
			states: "k1=unclear"}, // a stop hint forbids a retry but does not prove the write did not land
		{name: "an_unavailable_broken_hint_is_unclear",
			sends:  []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable, "soon"), acc()}, want: "unclear op1", calls: 1}},
			states: "k1=unclear"},
		{name: "multiple_pushback_hints_are_malformed_and_unclear",
			sends:  []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable, "1000", "-1"), acc()}, want: "unclear op1", calls: 1}},
			states: "k1=unclear"}, // several values are nonretryable and not an admission proof
		{name: "a_quota_refusal_is_failed_not_retried",
			sends:  []sendSpec{{script: []Attempt{grpcRej(codes.ResourceExhausted)}, want: "failed grpc-ResourceExhausted x1", calls: 1}},
			states: "k1=failed(grpc-ResourceExhausted)"}, // no header: a definite quota refusal
		{name: "cancel_during_backoff_sends_no_retry", setup: func(_ *testing.T, r *rig) {
			go func() { time.Sleep(time.Second); r.cancel() }() // inside the 5 s pushback wait
		}, sends: []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable), acc()}, want: "op1 dispatched=true softStop", calls: 1}},
			states: "k1=unclear", took: "1s"},
		{name: "pushback_waits_at_least_the_floor_the_hint_and_the_backoff", setup: func(_ *testing.T, r *rig) {
			start := time.Now()
			order = nil
			r.onAttempt = func(context.Context) { order = append(order, time.Since(start).String()) }
		}, sends: []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable, "7000"), grpcRej(codes.Unavailable, "10"), acc()},
			want: "accepted x3", calls: 3}}, states: "k1=settledx3", took: "12s", check: func(t *testing.T, _ *rig) {
			if got := strings.Join(order, " "); got != "0s 7s 12s" { // 7 s hint over the 5 s floor, then the 5 s floor
				t.Fatal(got)
			}
		}},
		{name: "an_attempt_ends_at_its_timeout", sends: []sendSpec{{tweak: func(c *Call) {
			c.Idempotent, c.Retry.AttemptTimeout = false, 20*time.Second
		}, script: []Attempt{pending}, want: "unclear op1", calls: 1}}, states: "k1=unclear", took: "20s"},
		{name: "an_attempt_ends_on_cancel", setup: func(_ *testing.T, r *rig) {
			go func() { time.Sleep(3 * time.Second); r.cancel() }()
		}, sends: []sendSpec{{tweak: nonIdem, script: []Attempt{pending}, want: "unclear op1", calls: 1}},
			states: "k1=unclear", took: "3s"},
		{name: "an_attempt_already_past_its_timeout_is_never_called", sends: []sendSpec{{tweak: func(c *Call) {
			c.Idempotent, c.Retry.AttemptTimeout = false, 0
		}, script: []Attempt{acc()}, want: "op1 dispatched=false deadline", calls: 0}}, states: "k1=notDispatched"},
		{name: "a_retry_wait_ends_on_a_soft_stop", setup: func(_ *testing.T, r *rig) {
			go func() { time.Sleep(time.Second); r.stop.stop() }() // well inside the 5 s pushback wait
		}, sends: []sendSpec{{script: []Attempt{grpcRej(codes.Unavailable), acc()}, want: "op1 dispatched=true softStop", calls: 1}},
			states: "k1=unclear", took: "1s"},
		{name: "waits_are_capped_and_never_overflow", sends: []sendSpec{{tweak: func(c *Call) {
			c.Retry.BaseBackoff, c.Retry.MaxBackoff = math.MaxInt64, 2*time.Second
		}, script: []Attempt{rejn(journal.Pushback("grpc-Unavailable"), true, sec(86_400)), acc()}, want: "accepted x2", calls: 2}},
			states: "k1=settledx2", took: "1m0s"}, // the 24 h hint is cut to MaxRetryAfter
		{name: "no_retry_waits_past_the_deadline", setup: func(_ *testing.T, r *rig) {
			r.w = New(r.j, r.gate, r.stop, time.Now().Add(500*time.Millisecond))
		}, sends: []sendSpec{{script: []Attempt{grpcRej(codes.ResourceExhausted, "100"), acc()}, want: "failed pushback-grpc-ResourceExhausted x1", calls: 1}},
			states: "k1=failed(pushback-grpc-ResourceExhausted)", took: "0s"}, // the backoff passes the 0.5 s deadline: no retry
		{name: "a_retry_stalled_past_the_cutoff_after_a_pushback_stays_failed", setup: stallRetry, sends: []sendSpec{{tweak: cutoffAt6s,
			script: []Attempt{grpcRej(codes.ResourceExhausted, "100"), acc()}, want: "failed pushback-grpc-ResourceExhausted x1", calls: 1}},
			states: "k1=failed(pushback-grpc-ResourceExhausted)x2", check: afterStall(journal.Counts{Ops: 1, Attempts: 1, Known: true}, false)},
		{name: "a_retry_stalled_past_the_cutoff_after_no_reply_stays_unclear", setup: stallRetry, sends: []sendSpec{{tweak: cutoffAt6s,
			script: []Attempt{noReply(), acc()}, want: "unclear op1", calls: 1}},
			states: "k1=unclearx2", check: afterStall(journal.Counts{Ops: 1, Attempts: 1, Unclear: 1, Known: true}, true)},
		{name: "a_detail_never_reaches_the_journal", sends: []sendSpec{{script: []Attempt{{
			Rejected: &Rejection{Reason: journal.Grpc("InvalidArgument")},
			Detail:   "https://user:pw@api.example/v1?token=abc " + strings.Repeat("x", 10_000),
		}}, want: "failed grpc-InvalidArgument x1", calls: 1}}, states: "k1=failed(grpc-InvalidArgument)",
			check: func(t *testing.T, r *rig) {
				raw, _ := os.ReadFile(r.j.Path())
				if strings.Contains(string(raw), "example") || strings.Contains(string(raw), "token") || strings.Contains(string(raw), "xxx") {
					t.Fatal("reply text in the journal:", string(raw))
				}
			}},
		{name: "a_used_key_refuses_a_later_send", sends: []sendSpec{
			{script: []Attempt{grpcRej(codes.ResourceExhausted, "100"), acc()}, want: "accepted x2", calls: 2},
			{script: []Attempt{acc()}, want: "keyBusy", calls: 0}, // one op per key for the run
		}, states: "k1=settledx2"},
		{name: "a_receipt_sync_failure_breaks_the_journal", setup: func(_ *testing.T, r *rig) {
			r.onAttempt = func(context.Context) { r.j.Inject(failSync); r.onAttempt = nil }
		}, sends: []sendSpec{
			{script: []Attempt{acc()}, want: "op1 dispatched=true io", calls: 1},
			{key: "k2", script: []Attempt{acc()}, want: "io", calls: 0},
		}, states: "k1=unclear"}, // the receipt the disk may drop is never read as settled
		{name: "a_partial_intent_write_breaks_the_journal", setup: func(_ *testing.T, r *rig) {
			r.j.Inject(func(f *os.File, line []byte) error {
				_, _ = f.Write(line[:20])
				return errors.New("injected: no space")
			})
		}, sends: []sendSpec{
			{script: []Attempt{acc()}, want: "io", calls: 0}, // the first intent fails: no op yet
			{key: "k2", script: []Attempt{acc()}, want: "io", calls: 0},
		}, states: ""}, // nothing dispatched, the torn line is left out
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				r := newRig(t)
				if row.setup != nil {
					row.setup(t, r)
				}
				start := time.Now()
				r.calls = make([]int32, len(row.sends))
				for i, s := range row.sends {
					got, n := r.send(t, s)
					r.calls[i] = n
					if got != s.want {
						t.Fatalf("send %d: got %q, want %q", i, got, s.want)
					}
					if n != s.calls {
						t.Fatalf("send %d: %d attempts, want %d", i, n, s.calls)
					}
				}
				if row.took != "" {
					if got := time.Since(start).String(); got != row.took {
						t.Fatalf("took %s, want %s", got, row.took)
					}
				}
				if got := r.states(t); got != row.states {
					t.Fatalf("states %q, want %q", got, row.states)
				}
				if row.check != nil {
					row.check(t, r)
				}
			})
		})
	}
}

// TestPlan proves the dry-run path journals the intent, settles it with a planned receipt (never an accepted one), keeps the gate and
// stop checks and the journal-before-dispatch order, and counts a planned op with no attempt.
func TestPlan(t *testing.T) {
	closeGate := func(r *rig) { r.gate.Close() }
	onIntent := func(f func(*rig)) func(*rig) {
		return func(r *rig) {
			r.j.AfterSync = func(e any) {
				if _, ok := e.(journal.Intent); ok {
					f(r)
				}
			}
		}
	}
	for _, row := range []struct {
		name, want, entries string
		setup               func(*rig)
		counts              journal.Counts
		unsettled           bool
	}{
		{"planned", "planned op1", "intent planned", func(*rig) {}, journal.Counts{Ops: 1, Known: true}, false},
		{"lock_lost_before_the_intent", "lockLost", "", closeGate, journal.Counts{Known: true}, false},
		{"lock_lost_during_the_intent_sync", "op1 dispatched=false lockLost", "intent notDispatched", onIntent(closeGate), journal.Counts{Ops: 1, Known: true}, false},
		{"stop_during_the_intent_sync", "op1 dispatched=false softStop", "intent notDispatched", onIntent(func(r *rig) { r.stop.stop() }), journal.Counts{Ops: 1, Known: true}, false},
		{"receipt_io_failure", "op1 dispatched=false io", "intent", onIntent(func(r *rig) { r.j.Inject(failSync) }), journal.Counts{Ops: 1, Attempts: 1, Unclear: 1}, true},
	} {
		t.Run(row.name, func(t *testing.T) {
			r := newRig(t)
			row.setup(r)
			out, err := Plan(r.ctx, r.w, mkCall("k1", "d1", nil))
			var got []string
			for _, m := range rawLines(t, r.j) {
				switch m["entry"] {
				case "intent":
					got = append(got, "intent")
				case "receipt":
					got = append(got, m["outcome"].(map[string]any)["kind"].(string))
				}
			}
			if g := show(out, err); g != row.want || strings.Join(got, " ") != row.entries || r.j.Counts() != row.counts || r.j.Unsettled() != row.unsettled {
				t.Fatalf("got %q entries %q counts %+v unsettled %t", g, got, r.j.Counts(), r.j.Unsettled())
			}
		})
	}
}

// stuckCtx reports a deadline that its Err and Done do not yet reflect, as a
// context whose deadline timer has not fired. It exercises the caller-deadline
// clamp, which must not rely on the context's own notification.
type stuckCtx struct {
	context.Context
	deadline time.Time
}

func (c stuckCtx) Deadline() (time.Time, bool) { return c.deadline, true }

// TestCallerDeadlineExpiredBeforeDispatch: an already-elapsed caller deadline
// dispatches nothing, even when the context has not yet reported it.
func TestCallerDeadlineExpiredBeforeDispatch(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		ctx := stuckCtx{context.Background(), time.Now().Add(-time.Second)}
		var calls atomic.Int32
		out, err := Send(ctx, r.w, mkCall("k1", "d1", nil), func(context.Context) Attempt {
			calls.Add(1)
			return acc()
		})
		r.w.Close()
		if calls.Load() != 0 {
			t.Fatal("dispatched past an elapsed caller deadline")
		}
		if got := show(out, err); got != "op1 dispatched=false softStop" {
			t.Fatal(got)
		}
	})
}

// TestCallerDeadlineDuringCallbackIsUnclear: a caller deadline that elapses
// while an attempt that ignores its context runs makes the op unclear, never
// accepted, even when the context has not reported the deadline.
func TestCallerDeadlineDuringCallbackIsUnclear(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		ctx := stuckCtx{context.Background(), time.Now().Add(time.Second)}
		var calls atomic.Int32
		out, err := Send(ctx, r.w, mkCall("k1", "d1", nonIdem), func(context.Context) Attempt {
			calls.Add(1)
			time.Sleep(2 * time.Second) // ignores its context, runs past the caller deadline
			return acc()
		})
		r.w.Close()
		if calls.Load() != 1 {
			t.Fatal("attempts:", calls.Load())
		}
		if got := show(out, err); got != "unclear op1" {
			t.Fatal(got)
		}
	})
}

// blockingGate blocks its Nth check until released, so the dispatch-time check
// waits with simulated time.
type blockingGate struct {
	n       atomic.Int32
	at      int32
	release chan struct{}
}

func (g *blockingGate) Check() error {
	if g.n.Add(1) == g.at {
		<-g.release
	}
	return nil
}

// TestDispatchTimeAfterBlockedGate: SentAt is the time of the actual dispatch,
// after the final gate check, not the earlier time the send queued.
func TestDispatchTimeAfterBlockedGate(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		gate := &blockingGate{at: 2, release: make(chan struct{})} // the dispatch-time check
		w := New(r.j, gate, r.stop, time.Time{})
		defer w.Close()
		start := time.Now()
		go func() { time.Sleep(3 * time.Second); close(gate.release) }()
		out, err := Send(r.ctx, w, mkCall("k1", "d1", nil), func(context.Context) Attempt { return acc() })
		w.Close()
		if got := show(out, err); got != "accepted x1" {
			t.Fatal(got)
		}
		if want := start.Add(3 * time.Second); !out.SentAt.Equal(want) {
			t.Fatalf("SentAt %s, want the post-block dispatch time %s", out.SentAt, want)
		}
	})
}

// TestSentAtIsTheFirstDispatchDespiteTimeout: when the first attempt times out
// and a later retry accepts, SentAt is the first attempt's dispatch time, not
// the retry's. A timed-out attempt reports its dispatch time, never zero.
func TestSentAtIsTheFirstDispatchDespiteTimeout(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		var mu sync.Mutex // a timed-out attempt's goroutine outlives its run call
		var entries []time.Time
		out, err := Send(r.ctx, r.w, mkCall("k1", "d1", func(c *Call) { c.Retry.AttemptTimeout = 2 * time.Second }),
			func(ctx context.Context) Attempt {
				mu.Lock()
				entries = append(entries, time.Now())
				first := len(entries) == 1
				mu.Unlock()
				if first {
					<-ctx.Done() // the first attempt times out
				}
				return acc()
			})
		r.w.Close()
		if got := show(out, err); got != "accepted x2" {
			t.Fatal(got)
		}
		if !out.SentAt.Equal(entries[0]) {
			t.Fatalf("SentAt %s, want the first dispatch %s (not the retry %s)", out.SentAt, entries[0], entries[1])
		}
	})
}

func opError(t *testing.T, err error) *OpError {
	t.Helper()
	var oe *OpError
	if !errors.As(err, &oe) {
		t.Fatalf("not an OpError: %v", err)
	}
	return oe
}

// TestReceiptIOReturnsKnownOutcome: a receipt that cannot be persisted still
// returns the known outcome, with an I/O OpError, so a caller can account for
// a write whose record was lost.
func TestReceiptIOReturnsKnownOutcome(t *testing.T) {
	armFailOnReply := func(r *rig) func(context.Context) Attempt {
		return func(context.Context) Attempt { r.j.Inject(failSync); return acc() }
	}
	t.Run("accepted", func(t *testing.T) {
		r := newRig(t)
		out, err := Send(r.ctx, r.w, mkCall("k1", "d1", nil), armFailOnReply(r))
		r.w.Close()
		if out.Kind != Accepted || !errors.Is(err, journal.ErrIO) || !opError(t, err).Dispatched {
			t.Fatalf("out %+v err %v", out, err)
		}
	})
	t.Run("definite_rejection_failed", func(t *testing.T) {
		r := newRig(t)
		out, err := Send(r.ctx, r.w, mkCall("k1", "d1", nil), func(context.Context) Attempt {
			r.j.Inject(failSync)
			return grpcRej(codes.InvalidArgument)
		})
		r.w.Close()
		if out.Kind != Failed || out.Reason != journal.Grpc("InvalidArgument") || !errors.Is(err, journal.ErrIO) {
			t.Fatalf("out %+v err %v", out, err)
		}
	})
	t.Run("rejection_after_an_earlier_lost_reply_is_unclear", func(t *testing.T) {
		r := newRig(t)
		var n atomic.Int32
		out, err := Send(r.ctx, r.w, mkCall("k1", "d1", nil), func(context.Context) Attempt {
			if n.Add(1) == 1 {
				return grpcRej(codes.Internal) // may have landed: a lost reply, retried
			}
			r.j.Inject(failSync)
			return grpcRej(codes.InvalidArgument) // definite rejection, but its receipt fails
		})
		r.w.Close()
		if out.Kind != Unclear || !errors.Is(err, journal.ErrIO) {
			t.Fatalf("out %+v err %v", out, err)
		}
	})
}

// TestCloseJoinsAReleasedCallback proves Close joins a callback that ignored
// its context, once the callback is released. It is not a bounded shutdown:
// Close waits for the callback, so an absolute watchdog belongs to the owner.
func TestCloseJoinsAReleasedCallback(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		r := newRig(t)
		release := make(chan struct{})
		out, err := Send(r.ctx, r.w, mkCall("k1", "d1", func(c *Call) { c.Idempotent, c.Retry.AttemptTimeout = false, time.Second }),
			func(context.Context) Attempt { <-release; return acc() }) // ignores its context
		if got := show(out, err); got != "unclear op1" {
			t.Fatal(got)
		}
		closed := make(chan struct{})
		go func() { r.w.Close(); close(closed) }()
		time.Sleep(time.Second)
		select {
		case <-closed:
			t.Fatal("Close returned while the callback was still running")
		default:
		}
		close(release)
		synctest.Wait()
		<-closed
	})
}
