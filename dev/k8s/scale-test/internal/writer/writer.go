// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package writer is the only path for a NICo write that returns Empty:
// journal the intent, recheck the lock and the stop signal, send, journal the
// receipt. A write whose reply is lost is marked unclear and never resent as
// if nothing had landed.
package writer

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
)

var (
	ErrLockLost   = errors.New("site lock lost")
	ErrSoftStop   = errors.New("soft stop requested")
	ErrUnclearKey = journal.ErrUnclearKey
	ErrKeyBusy    = journal.ErrKeyBusy
	// ErrCutoff: the call's Cutoff passed before an attempt could go out.
	ErrCutoff = errors.New("the dispatch cutoff passed")
	// errPastDeadline: a retry would go out past the run deadline.
	errPastDeadline = errors.New("a retry past the run deadline")
	// errDeadline: the attempt's own timeout passed before it dispatched.
	errDeadline = errors.New("attempt deadline passed")
)

const (
	// MaxRetryAfter is the longest server retry hint honoured.
	MaxRetryAfter = time.Minute
	// MaxDetailChars is the longest detail logged per attempt.
	MaxDetailChars = 256
)

// RetryPolicy is how the writer retries one call.
type RetryPolicy struct {
	MaxAttempts             uint32
	BaseBackoff, MaxBackoff time.Duration
	PushbackDelay           time.Duration // the least wait after a pushback
	AttemptTimeout          time.Duration // no reply by then is a lost reply
}

// DefaultRetry: 5 attempts, 1 s to 30 s backoff, 5 s pushback (the admission
// default), 60 s per attempt.
var DefaultRetry = RetryPolicy{5, time.Second, 30 * time.Second, 5 * time.Second, time.Minute}

// Call is one logical write and how to retry it.
type Call struct {
	journal.WriteCall
	Retry RetryPolicy
	// Cutoff, when set: no attempt, the first included, goes out at or after it, checked last, right before the dispatch. An attempt already out is not cut.
	Cutoff time.Time
}

func (c Call) cut() bool { return !c.Cutoff.IsZero() && !time.Now().Before(c.Cutoff) }

// Gate is the site lock's write gate: Check fails once the lock is lost. The
// lock package (a later PR) provides it; the writer depends only on this.
type Gate interface{ Check() error }

// Stopper signals a soft stop: finish work in flight, dispatch nothing new.
// Its channel is closed on a stop. A later PR provides the run's stop; the
// writer depends only on this.
type Stopper interface{ Stopped() <-chan struct{} }

// Attempt is what one dispatched attempt got back, for a write that returns
// Empty: accepted when neither Rejected nor NoReply. Detail is local text the
// adapter writes, never a reply body or an error that can hold a URL: the
// writer only logs it, scrubbed and cut, and the journal keeps the reason
// alone.
type Attempt struct {
	Rejected *Rejection
	NoReply  bool // the write may or may not have landed
	Detail   string
}

// Kind is how an op ended.
type Kind uint8

const (
	Accepted Kind = iota + 1
	Failed
	Unclear // a lost reply that may not be resent: the op stays unclear
	Planned // a dry run journaled the write and sent nothing
)

// Outcome of one logical write. SentAt (of attempt 1) and RepliedAt are set
// when Accepted, Reason when Failed.
type Outcome struct {
	Kind              Kind
	Op                uint64
	Attempts          uint32
	Reason            journal.Reason
	SentAt, RepliedAt time.Time
}

// OpError is an error that came after the send journaled an intent of Op.
// When Dispatched, an attempt of the op went out and may have landed.
type OpError struct {
	Op         uint64
	Dispatched bool
	Err        error
}

func (e *OpError) Error() string {
	return fmt.Sprintf("op %d (dispatched: %t): %v", e.Op, e.Dispatched, e.Err)
}
func (e *OpError) Unwrap() error { return e.Err }

// Writer sends gated, journaled writes.
type Writer struct {
	j    *journal.Journal
	gate Gate
	stop Stopper
	// No retry waits past it, or goes out after it: the run's deadline. Zero
	// for none.
	deadline time.Time
	attempts sync.WaitGroup // attempt calls, which may outlive their bound
}

func New(j *journal.Journal, gate Gate, stop Stopper, deadline time.Time) *Writer {
	return &Writer{j: j, gate: gate, stop: stop, deadline: deadline}
}

// Close joins the attempt calls that outlived their timeout or a cancel.
// Their contexts are already done, so a cooperative one has returned. The
// owner defers it.
func (w *Writer) Close() { w.attempts.Wait() }

// halted: the lock is lost, or a stop, a cancel (also a stop) or the end of
// ctx says to send nothing new.
func (w *Writer) halted(ctx context.Context) error {
	if w.gate.Check() != nil {
		return ErrLockLost
	} else if ctx.Err() != nil || w.stopped() {
		return ErrSoftStop
	}
	return nil
}

func (w *Writer) stopped() bool {
	select {
	case <-w.stop.Stopped():
		return true
	default:
		return false
	}
}

// pastDeadline: a retry after wait would start past the deadline.
func (w *Writer) pastDeadline(wait time.Duration) bool {
	return !w.deadline.IsZero() && !time.Now().Add(wait).Before(w.deadline)
}

// Send sends one logical write, retrying per c.Retry.
//
// Each attempt: check the gate, persist the intent, recheck the gate and the
// stop signal, then call attempt. A failed intent is never dispatched. A
// closed gate or a stop after the intent writes notDispatched and attempt is
// never called. A soft stop never reaches an attempt in flight: only ctx
// does, or the attempt timeout, and either is a lost reply. A rejection that
// does not prove the write was not applied is a lost reply too. A lost reply
// of a non-idempotent call, or of a permanent rejection, is never resent. A
// pushback waits at least the pushback delay, the server's hint (cut to
// MaxRetryAfter) and the backoff. A wait ends early on a stop or a cancel,
// and no retry waits past the deadline or goes out after it. An error after
// an intent is an *OpError.
func Send(ctx context.Context, w *Writer, c Call, attempt func(context.Context) Attempt) (out Outcome, err error) {
	// One logical op per key for the run: a second send of a used key is
	// refused, so a settled or unclear op is never reopened by another send.
	if err := w.j.Claim(c.Key); err != nil {
		return out, err
	}
	var (
		op          uint64 // of the last intent
		dispatched  bool   // an attempt of op went out
		firstSentAt time.Time
		retrying    journal.Reason // why the next dispatch is a retry
	)
	defer func() {
		if err != nil && op != 0 {
			err = &OpError{op, dispatched, err}
		}
	}()
	backoff := min(c.Retry.BaseBackoff, c.Retry.MaxBackoff)
	for {
		if err := w.halted(ctx); err != nil {
			return out, err
		}
		in, err := w.j.Intent(c.WriteCall)
		if err != nil {
			return out, err
		}
		op, dispatched = in.Op, in.Attempt > 1
		retry := retrying // why this dispatch is a retry, within this send
		// The gate, the stop and a retry's deadline are checked last at the
		// dispatch itself, after every lock and bookkeeping step. Nothing
		// waits on the journal between this check and the dispatch. run reports
		// the actual dispatch time, taken after that final check.
		reply, dispatchAt, halt := run(w, ctx, c.Retry.AttemptTimeout, func() error {
			if err := w.halted(ctx); err != nil {
				return err
			} else if c.cut() {
				return ErrCutoff
			} else if in.Attempt == 1 || !w.pastDeadline(0) {
				return nil
			}
			return errPastDeadline
		}, attempt)
		repliedAt := time.Now().UTC()
		if halt != nil {
			lost := w.j.LostReply(in.Op)
			if err := w.j.Receipt(in, journal.Outcome{Kind: journal.NotDispatched}); err != nil {
				return out, err
			} else if !errors.Is(halt, errPastDeadline) && (!errors.Is(halt, ErrCutoff) || in.Attempt == 1) {
				return out, halt
			}
			// A retry never goes out past the deadline or the cutoff, however
			// long the journal took: the op ends with what its last attempt got.
			if lost {
				return Outcome{Kind: Unclear, Op: in.Op}, nil
			}
			retry = cmp.Or(retry, journal.Timeout)
			return Outcome{Kind: Failed, Op: in.Op, Reason: retry, Attempts: in.Attempt - 1}, nil
		}
		dispatched = true
		if firstSentAt.IsZero() && !dispatchAt.IsZero() {
			firstSentAt = dispatchAt
		}
		// A rejection that may have come after the write landed is a lost
		// reply, retried, when it may be, for its own reason and hint.
		lost := Rejection{Reason: journal.Timeout, Retryable: true}
		if r := reply.Rejected; r != nil && maybeApplied(*r) {
			lost, reply = *r, Attempt{NoReply: true, Detail: string(r.Reason) + ": the write may have landed"}
		}
		var rej Rejection
		switch {
		case reply.NoReply:
			slog.Warn("write got no reply", "scope", c.Scope, "kind", c.Kind, "key", c.Key, "detail", scrubbed(reply.Detail))
			w.j.MarkUnclear(in.Op)
			if !c.Idempotent || !lost.Retryable || in.Attempt >= c.Retry.MaxAttempts {
				return Outcome{Kind: Unclear, Op: in.Op}, nil
			}
			rej = Rejection{Reason: lost.Reason, Retryable: true, RetryAfter: lost.RetryAfter}
		case reply.Rejected != nil:
			// A definite rejection. An op where an earlier attempt lost its
			// reply stays unclear; otherwise an unretried rejection is a
			// failure. The known outcome is captured before the receipt so a
			// receipt I/O failure still reports it.
			rej = *reply.Rejected
			slog.Debug("write rejected", "scope", c.Scope, "kind", c.Kind, "key", c.Key, "reason", rej.Reason, "detail", scrubbed(reply.Detail))
			known := Outcome{Kind: Failed, Op: in.Op, Reason: rej.Reason, Attempts: in.Attempt}
			if w.j.LostReply(in.Op) {
				known = Outcome{Kind: Unclear, Op: in.Op}
			}
			if err := w.j.Receipt(in, journal.Outcome{Kind: journal.Rejected, Reason: rej.Reason}); err != nil {
				return known, err
			}
		default:
			slog.Info("write accepted", "scope", c.Scope, "kind", c.Kind, "key", c.Key, "op", in.Op)
			out = Outcome{Kind: Accepted, Op: in.Op, Attempts: in.Attempt, SentAt: firstSentAt, RepliedAt: repliedAt}
			if err := w.j.Receipt(in, journal.Outcome{Kind: journal.Accepted}); err != nil {
				return out, err // the known acceptance, with the I/O error
			}
			return out, nil
		}
		wait := backoff
		if rej.RetryAfter != nil {
			wait = max(wait, min(*rej.RetryAfter, MaxRetryAfter))
		}
		if rej.pushback() {
			wait = max(wait, c.Retry.PushbackDelay)
		}
		if !rej.Retryable || in.Attempt >= c.Retry.MaxAttempts || w.pastDeadline(wait) {
			// An attempt of this op, in this send or an earlier one, lost its
			// reply and may still have landed.
			if w.j.CloseFailed(in.Op) {
				return Outcome{Kind: Unclear, Op: in.Op}, nil
			}
			return Outcome{Kind: Failed, Op: in.Op, Reason: rej.Reason, Attempts: in.Attempt}, nil
		}
		retrying, backoff = rej.Reason, nextBackoff(backoff, c.Retry.MaxBackoff)
		timer := time.NewTimer(wait)
		select {
		case <-timer.C:
		case <-w.stop.Stopped():
		case <-ctx.Done():
		}
		timer.Stop()
	}
}

// Plan is the dry-run Send: nothing is dispatched and no attempt exists. It
// claims the key, checks the gate and the stop signal, persists the intent,
// rechecks, and settles the op with a planned receipt, never an accepted one. A
// halt or the cutoff after the intent writes notDispatched. An error after the intent is an
// *OpError, never Dispatched.
func Plan(ctx context.Context, w *Writer, c Call) (out Outcome, err error) {
	if err = w.j.Claim(c.Key); err != nil {
		return out, err
	}
	if err = w.halted(ctx); err != nil {
		return out, err
	}
	in, err := w.j.Intent(c.WriteCall)
	if err != nil {
		return out, err
	}
	defer func() {
		if err != nil {
			err = &OpError{Op: in.Op, Err: err}
		}
	}()
	halt := w.halted(ctx)
	if halt == nil && c.cut() {
		halt = ErrCutoff
	}
	if halt != nil {
		if err = w.j.Receipt(in, journal.Outcome{Kind: journal.NotDispatched}); err == nil {
			err = halt
		}
		return out, err
	}
	if err = w.j.Receipt(in, journal.Outcome{Kind: journal.Planned}); err != nil {
		return out, err
	}
	slog.Info("write planned", "scope", c.Scope, "kind", c.Kind, "key", c.Key, "op", in.Op)
	return Outcome{Kind: Planned, Op: in.Op}, nil
}

// run calls attempt under the attempt timeout and ctx, unless halt, run right
// before the call, says not to, or the bound or ctx end first: run then
// returns why, and attempt is never called. The bound is the attempt timeout,
// clamped to an earlier caller deadline so a stuck caller timer cannot let a
// call past its deadline go out or be accepted. run reports the actual
// dispatch time, taken inside the attempt goroutine just before the call. It
// never waits past the bound or ctx, even for an attempt that ignores its
// context: that is a lost reply. A cancel or the bound comes first, so a reply
// ready at the same time is lost too.
func run(w *Writer, ctx context.Context, timeout time.Duration, halt func() error, attempt func(context.Context) Attempt) (Attempt, time.Time, error) {
	attemptUntil := time.Now().Add(timeout)
	until := attemptUntil
	if d, ok := ctx.Deadline(); ok && d.Before(until) {
		until = d // the caller's deadline bounds the attempt
	}
	// tooLate: a caller cancel or an elapsed caller deadline is a caller stop;
	// the attempt's own timeout is errDeadline.
	tooLate := func() error {
		if ctx.Err() != nil {
			return ErrSoftStop
		}
		if !time.Now().Before(until) {
			if until.Before(attemptUntil) {
				return ErrSoftStop // the caller's deadline, a caller stop
			}
			return errDeadline
		}
		return nil
	}
	actx, cancel := context.WithDeadline(ctx, until)
	defer cancel()
	var claim atomic.Int32                   // 1: attempt is called, 2: run gave up before it
	var dispatchAt atomic.Pointer[time.Time] // the immutable dispatch time, set once at the claim
	done := make(chan sent, 1)
	w.attempts.Go(func() {
		err := halt()
		if err == nil {
			err = tooLate()
		}
		if err != nil {
			done <- sent{err: err}
			return
		}
		// Publish the dispatch time before the claim, so a dispatched attempt
		// whose reply is lost still reports when it went out. A claim run won
		// first means this attempt never ran, and run returns a zero time
		// without reading this pointer.
		at := time.Now().UTC()
		dispatchAt.Store(&at)
		if claim.CompareAndSwap(0, 1) {
			done <- sent{reply: attempt(actx)}
		} else {
			done <- sent{err: tooLate()}
		}
	})
	select {
	case r := <-done:
		if r.err != nil || tooLate() == nil {
			return r.reply, sentAt(&dispatchAt), r.err
		}
	case <-actx.Done():
		if claim.CompareAndSwap(0, 2) {
			return Attempt{}, time.Time{}, tooLate() // run won the claim: never dispatched
		}
	}
	// Dispatched, but no reply in time: keep the original dispatch time.
	return Attempt{NoReply: true, Detail: "no reply within the attempt timeout or before a cancel"}, sentAt(&dispatchAt), nil
}

// sent is one attempt's result, or why it did not go out.
type sent struct {
	reply Attempt
	err   error
}

// sentAt is the published dispatch time, or the zero time when the attempt was
// never claimed.
func sentAt(p *atomic.Pointer[time.Time]) time.Time {
	if at := p.Load(); at != nil {
		return *at
	}
	return time.Time{}
}

// nextBackoff doubles b, never past max or overflowing.
func nextBackoff(b, max time.Duration) time.Duration {
	if b > math.MaxInt64/2 {
		return max
	}
	return min(2*b, max)
}

// scrubbed is detail as a log may show it: a word that may hold a URL, an
// address, a query or a credential is removed, control characters are
// dropped, and the text is cut to MaxDetailChars.
func scrubbed(detail string) string {
	words := strings.Fields(detail)
	for i, word := range words {
		if strings.Contains(word, "://") || strings.ContainsAny(word, "@?=&") {
			words[i] = "[removed]"
		}
	}
	clean := []rune(strings.Map(func(r rune) rune {
		if unicode.IsControl(r) {
			return -1
		}
		return r
	}, strings.Join(words, " ")))
	if len(clean) > MaxDetailChars {
		return string(clean[:MaxDetailChars]) + "..."
	}
	return string(clean)
}
