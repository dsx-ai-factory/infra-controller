// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// ForgeWriter is the five expected-inventory writes of nico-api. forge.Client satisfies it.
type ForgeWriter interface {
	AddExpectedRackGroup(ctx context.Context, body *corev1.ExpectedRackGroup, opts ...grpc.CallOption) error
	AddExpectedRack(ctx context.Context, body *corev1.ExpectedRack, opts ...grpc.CallOption) error
	AddExpectedMachine(ctx context.Context, body *corev1.ExpectedMachine, opts ...grpc.CallOption) error
	AddExpectedSwitch(ctx context.Context, body *corev1.ExpectedSwitch, opts ...grpc.CallOption) error
	AddExpectedPowerShelf(ctx context.Context, body *corev1.ExpectedPowerShelf, opts ...grpc.CallOption) error
}

// Concurrency is how many writes of each stream are in flight at once.
type Concurrency struct {
	Rack       int `yaml:"rack"`
	Host       int `yaml:"host"`
	Switch     int `yaml:"switch"`
	PowerShelf int `yaml:"powerShelf"`
}

// DefaultConcurrency is 8 writes in flight per stream.
var DefaultConcurrency = Concurrency{8, 8, 8, 8}

func (c Concurrency) of(s Stream) int {
	switch s {
	case Host:
		return c.Host
	case Switch:
		return c.Switch
	case PowerShelf:
		return c.PowerShelf
	}
	return c.Rack
}

// Status is how a planned key ended.
type Status string

const (
	Sent             Status = "sent"             // accepted
	Planned          Status = "planned"          // dryRun: journaled, its prerequisites taken as accepted, nothing sent
	ExistedBeforeRun Status = "existedBeforeRun" // AlreadyExists on a key with no lost reply: the record predates the run
	Rejected         Status = "rejected"
	Unclear          Status = "unclear" // may have landed
	NotSent          Status = "notSent"
	GroupNotAccepted Status = "groupNotAccepted"
	RackNotAccepted  Status = "rackNotAccepted"
)

// accepted: the key was accepted, or planned under dryRun, so the writes that wait on it may go.
func accepted(s Status) bool { return s == Sent || s == Planned }

// KeyOutcome is one planned key's end. Reason is the rejection label when Rejected; the times and attempts are the writer's.
type KeyOutcome struct {
	Status            Status
	Reason            string
	Attempts          uint32
	SentAt, RepliedAt time.Time
}

// Results holds one outcome per planned key, the first error that stopped dispatch and each dispatched stream's window.
type Results struct {
	Outcomes map[WriteKey]KeyOutcome
	Err      error
	Windows  map[Stream]Window
}

// Window is a stream's dispatch window. T0 is its first unit attempt to go out, past the writer's last gate, stop and cutoff check (a rack group is a prerequisite,
// not timed), set even when that attempt is rejected. End is the last accepted reply, or the later return of an unclear send, which may have landed by then;
// zero when neither came. A stream with no attempt out has no T0.
type Window struct{ T0, End time.Time }

type dispatcher struct {
	ctx    context.Context
	w      *writer.Writer
	f      ForgeWriter
	recs   map[WriteKey]Record
	until  time.Time // no send starts at or past it, when set
	dryRun bool

	mu  sync.Mutex
	out map[WriteKey]KeyOutcome
	err error // the first error that stops dispatch: no send starts once it is set
	win map[Stream]Window
}

// Dispatch sends every planned write through the writer: each rack's group and then, once the group is accepted, the rack, at most conc.Rack racks at a time;
// then the host, switch and power shelf streams in parallel, each at most its concurrency. A device naming a rack that was not accepted is not sent. The first
// lock loss, stop or journal error stops every later send, and no attempt goes out at or past a nonzero deadline, the writer's last check before the RPC; the RPCs already out finish. Under dryRun every
// write is journaled, none is dispatched and an accepted key is planned. Bad input is an error with every key notSent.
func Dispatch(ctx context.Context, w *writer.Writer, f ForgeWriter, plan WritePlan, recs map[WriteKey]Record, conc Concurrency, deadline time.Time, dryRun bool) Results {
	d := &dispatcher{ctx: ctx, w: w, f: f, recs: recs, until: deadline, dryRun: dryRun, out: map[WriteKey]KeyOutcome{}, win: map[Stream]Window{}}
	keys := plan.keys()
	if err := d.check(keys, conc); err != nil {
		d.err = err
	} else {
		each(len(plan.Racks), conc.Rack, func(i int) {
			r := plan.Racks[i]
			switch s := d.send(r.Group).Status; {
			case accepted(s):
				d.send(r.Rack)
			case s == NotSent:
				d.set(r.Rack, KeyOutcome{Status: NotSent})
			case !d.stopped():
				d.set(r.Rack, KeyOutcome{Status: GroupNotAccepted})
			}
		})
		d.devices(plan, conc)
	}
	for _, k := range keys {
		if _, ok := d.out[k]; !ok {
			d.out[k] = KeyOutcome{Status: NotSent}
		}
	}
	return Results{Outcomes: d.out, Err: d.err, Windows: d.win}
}

// devices sends the three device streams in parallel, unless the racks stopped dispatch. The rack outcomes are final: every rack send has returned.
func (d *dispatcher) devices(plan WritePlan, conc Concurrency) {
	if d.stopped() {
		return
	}
	racks := map[string]Status{}
	for _, r := range plan.Racks {
		racks[r.RackID] = d.out[r.Rack].Status
	}
	var streams sync.WaitGroup
	for _, s := range []Stream{Host, Switch, PowerShelf} {
		devices := plan.Devices[s]
		streams.Go(func() {
			each(len(devices), conc.of(s), func(i int) {
				// A device naming no rack has no rack to wait for.
				if dw := devices[i]; dw.RackID != "" && racks[dw.RackID] == NotSent {
					d.set(dw.Key, KeyOutcome{Status: NotSent})
				} else if dw.RackID != "" && !accepted(racks[dw.RackID]) {
					d.set(dw.Key, KeyOutcome{Status: RackNotAccepted})
				} else {
					d.send(dw.Key)
				}
			})
		})
	}
	streams.Wait()
}

// check refuses input that would panic or send a wrong body: a missing writer or forge, a concurrency under 1, or a planned key with no body of its kind. An
// error names the write's index, never its key: a key holds a MAC.
func (d *dispatcher) check(keys []WriteKey, conc Concurrency) error {
	switch {
	case d.w == nil, d.f == nil && !d.dryRun:
		return errors.New("ingestion dispatch needs a writer and a nico-api client")
	case min(conc.Rack, conc.Host, conc.Switch, conc.PowerShelf) < 1:
		return errors.New("every write concurrency must be at least 1")
	}
	for n, k := range keys {
		rec := d.recs[k]
		if kind := kindOf(rec.Body); kind == "" || kind != rec.Kind || !rec.Body.ProtoReflect().IsValid() {
			return fmt.Errorf("a planned write has no body of its kind (write %d)", n)
		}
	}
	return nil
}

// each runs job(0..n-1), at most limit at once, and waits for them.
func each(n, limit int, job func(int)) {
	var wg sync.WaitGroup
	slots := make(chan struct{}, limit)
	for i := range n {
		slots <- struct{}{}
		wg.Go(func() {
			defer func() { <-slots }()
			job(i)
		})
	}
	wg.Wait()
}

func (d *dispatcher) stopped() bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.err != nil
}

func (d *dispatcher) set(k WriteKey, o KeyOutcome) KeyOutcome {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.out[k] = o
	return o
}

// send sends one key through the writer and records its outcome. A key refused as busy or unclear is unclear and dispatch goes on, as does one the writer's cutoff stopped (notSent); any other error stops it.
func (d *dispatcher) send(k WriteKey) KeyOutcome {
	if d.stopped() || !d.until.IsZero() && !time.Now().Before(d.until) {
		return d.set(k, KeyOutcome{Status: NotSent})
	}
	rec := d.recs[k]
	// Up to 30 attempts for an expected-record write (spec-E 1.5), with the writer's default backoff, pushback floor and deadlines; its generic default is 5.
	retry := writer.DefaultRetry
	retry.MaxAttempts = 30
	call := writer.Call{WriteCall: journal.WriteCall{Scope: "test", Kind: rec.Kind, Key: string(k), Idempotent: true, RequestDigest: rec.Digest}, Retry: retry, Cutoff: d.until}
	send := func() (writer.Outcome, error) {
		return writer.Send(d.ctx, d.w, call, func(ctx context.Context) writer.Attempt {
			d.window(k, time.Now(), false)
			return attempt(ctx, d.f, rec.Body)
		})
	}
	if d.dryRun {
		send = func() (writer.Outcome, error) { return writer.Plan(d.ctx, d.w, call) }
	}
	out, err := send()
	if err != nil && !errors.Is(err, journal.ErrKeyBusy) && !errors.Is(err, journal.ErrUnclearKey) && !errors.Is(err, writer.ErrCutoff) {
		d.mu.Lock()
		if d.err == nil {
			d.err = err
		}
		d.mu.Unlock()
	}
	o := account(out, err)
	switch o.Status {
	case Sent:
		d.window(k, o.RepliedAt, true)
	case Unclear:
		d.window(k, time.Now(), true)
	}
	return d.set(k, o)
}

// window opens the stream of k at its first dispatch, or moves the stream's end to at.
func (d *dispatcher) window(k WriteKey, at time.Time, end bool) {
	s, _, _ := strings.Cut(string(k), "/")
	if Stream(s) != Rack && Stream(s) != Host && Stream(s) != Switch && Stream(s) != PowerShelf {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	w := d.win[Stream(s)]
	switch {
	case !end && w.T0.IsZero():
		w.T0 = at
	case end && at.After(w.End):
		w.End = at
	}
	d.win[Stream(s)] = w
}

// account maps the writer's outcome to the key's. An error after a dispatched attempt leaves the key unclear: that attempt may have landed. The one exception is
// a known outcome, accepted or rejected, whose receipt then failed to persist: the writer returns it with the error, and the key keeps it. A planned key
// (dryRun) has no attempt, send or reply: nothing went out.
func account(out writer.Outcome, err error) KeyOutcome {
	var opErr *writer.OpError
	known := out.Kind == writer.Accepted || out.Kind == writer.Failed
	switch {
	case errors.Is(err, journal.ErrKeyBusy), errors.Is(err, journal.ErrUnclearKey), !known && errors.As(err, &opErr) && opErr.Dispatched:
		return KeyOutcome{Status: Unclear}
	case err != nil && !known:
		return KeyOutcome{Status: NotSent}
	}
	switch out.Kind {
	case writer.Planned:
		return KeyOutcome{Status: Planned}
	case writer.Accepted:
		return KeyOutcome{Status: Sent, Attempts: out.Attempts, SentAt: out.SentAt, RepliedAt: out.RepliedAt}
	case writer.Failed:
		if out.Reason == journal.Grpc("AlreadyExists") {
			return KeyOutcome{Status: ExistedBeforeRun, Attempts: out.Attempts}
		}
		return KeyOutcome{Status: Rejected, Reason: string(out.Reason), Attempts: out.Attempts}
	}
	// The writer makes any rejection after a lost reply of the key unclear. That includes AlreadyExists, on purpose (the writer's conservative policy: it does not say
	// which attempt or run created the record), where the Rust runner counted it as this run's write.
	return KeyOutcome{Status: Unclear}
}

// attempt sends body once and classifies the reply as nico-api's expected-record writes allow: nico-api validates a record before its transaction, so only a
// status it sent of AlreadyExists, FailedPrecondition (an NVOS MAC of another switch), InvalidArgument, PermissionDenied or Unauthenticated, or a
// ResourceExhausted carrying the admission pushback header, is a rejection. Every other status, and an error with no status, may have landed: a lost reply. Detail is fixed text with the code at most, never the status message.
// Two policies differ from the Rust runner's on purpose. AlreadyExists after a lost reply is unclear (the writer's conservative policy), where the Rust runner
// counted it as this run's write. A pushback with a negative or broken hint is a stop that ClassifyGRPC returns unretryable and the writer keeps unclear (the
// writer's maybeApplied: a stop hint does not prove the write did not land), where spec-E 1.5 counts it failed.
func attempt(ctx context.Context, f ForgeWriter, body proto.Message) writer.Attempt {
	var md metadata.MD
	opt := grpc.Trailer(&md)
	var err error
	switch b := body.(type) {
	case *corev1.ExpectedRackGroup:
		err = f.AddExpectedRackGroup(ctx, b, opt)
	case *corev1.ExpectedRack:
		err = f.AddExpectedRack(ctx, b, opt)
	case *corev1.ExpectedMachine:
		err = f.AddExpectedMachine(ctx, b, opt)
	case *corev1.ExpectedSwitch:
		err = f.AddExpectedSwitch(ctx, b, opt)
	case *corev1.ExpectedPowerShelf:
		err = f.AddExpectedPowerShelf(ctx, b, opt)
	default: // Dispatch checks every body first; never count an unsent write as accepted
		return writer.Attempt{Rejected: &writer.Rejection{Reason: journal.Grpc("Unimplemented")}, Detail: "the body is no expected-inventory write"}
	}
	if err == nil {
		return writer.Attempt{}
	}
	st, ok := status.FromError(err)
	if !ok {
		return writer.Attempt{NoReply: true, Detail: "the nico-api call failed with no gRPC status"}
	}
	detail := "nico-api replied " + st.Code().String()
	switch code := st.Code(); {
	case code == codes.AlreadyExists, code == codes.FailedPrecondition, code == codes.InvalidArgument, code == codes.PermissionDenied, code == codes.Unauthenticated,
		code == codes.ResourceExhausted && len(md.Get("grpc-retry-pushback-ms")) > 0:
		rej := writer.ClassifyGRPC(code, md)
		return writer.Attempt{Rejected: &rej, Detail: detail}
	}
	return writer.Attempt{NoReply: true, Detail: detail}
}
