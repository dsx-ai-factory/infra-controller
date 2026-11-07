// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package lock is the site lock: one Lease per site, so only one scale
// test, ingestion or fleet-up runs at a time.
//
// The Lease is cooperative exclusion, not server-side fencing. A request
// already sent when the lock is lost can still land. The journal records it,
// and an unclear write leaves the Lease needsCleanup.
//
// Renewal, SetState and Release share one mutex and replace the Lease with a
// compare-and-swap on the cached resource version and UID. Each write first
// checks that the Lease still names this exact holder and carries this
// acquire's token. A conflict re-reads: the same holder retries, another
// holder closes the gate at once and this process never writes that Lease
// again. A deleted Lease, seen as a 404 or as no Lease on the re-read, does
// the same, and so does a Lease with another UID: it was made again.
//
// Only acquire makes the Lease. The API makes a missing Lease on a replace
// unless the UID check refuses it, so a replace answered with 201, or with
// another UID, is a lost lock: the gate closes and the stray Lease, refused by
// its record or free once a release emptied it, is left for an operator.
//
// When renewals fail for the max renewal age the gate closes by itself, and
// needsCleanup is set only if one write on the cached version still lands.
//
// An acquire write whose reply is lost may still have committed. It is read
// back: a Lease that carries this attempt's token and names this holder is
// held. If the read back fails too, the run stops before its journal exists,
// and the Lease may still name it. That Lease expires and is then refused as
// a crashed run, with no journal.
//
// Recovery is by hand, one step at a time. Stop at the first that fails.
//
//  1. Inspect the Lease. Set LEASE_RV and LEASE_UID to its resource version
//     and UID, and read the holder record in the scale-test-holder
//     annotation: kubectl -n "$NS" get lease "$NAME" -o yaml. Not UID: the
//     shell already sets it to the user's ID.
//
//  2. Check that the Lease has expired, and that no pod of that execution
//     runs. The execution ID is the pod name, so
//     kubectl -n "$NS" get pod "$EXECUTION" must answer NotFound.
//
//  3. Check that the journal claim holds no journal of the run. A missing
//     journal alone proves nothing: a live process may still be about to
//     write it. A run with no journal and no live process never sent a write.
//
//  4. Delete the Lease only while it is the one inspected, with LEASE_RV and
//     LEASE_UID from step 1:
//
//     kubectl delete --raw "/apis/coordination.k8s.io/v1/namespaces/$NS/leases/$NAME" -f - <<EOF
//     {"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"resourceVersion":"$LEASE_RV","uid":"$LEASE_UID"}}
//     EOF
//
//     A 409 "Precondition failed" means the Lease changed after step 1: stop,
//     and start again at step 1. Plain kubectl delete checks no version.
package lock

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

const (
	// HolderAnnotation holds the JSON HolderRecord.
	HolderAnnotation = "nvidia-infra-controller/scale-test-holder"
	// TokenAnnotation holds the unique token of the acquire that wrote the
	// Lease. Only it proves a lost acquire reply was this attempt's.
	TokenAnnotation = "nvidia-infra-controller/scale-test-acquire-token"
)

// HolderKind and HolderState are closed sets as the lock writes them. An
// unknown value fails Decode, so the lock reads that holder as foreign.
type (
	HolderKind  string
	HolderState string
)

const (
	HolderTest      HolderKind  = "test"
	HolderIngestion HolderKind  = "ingestion"
	HolderFleetUp   HolderKind  = "fleet-up"
	HolderDeploy    HolderKind  = "deploy"
	Running         HolderState = "running"
	Final           HolderState = "final"
	NeedsCleanup    HolderState = "needsCleanup"
)

func init() {
	jsonx.Enum(HolderTest, HolderIngestion, HolderFleetUp, HolderDeploy)
	jsonx.Enum(Running, Final, NeedsCleanup)
}

// HolderRecord is who holds the Lease, kept as JSON in HolderAnnotation.
type HolderRecord struct {
	Kind        HolderKind  `json:"kind"`
	RunID       string      `json:"runId"`
	ExecutionID string      `json:"executionId"`
	State       HolderState `json:"state"`
	Test        *string     `json:"test"`
}

// JSONClosed makes jsonx.Decode refuse a field the record does not have.
func (HolderRecord) JSONClosed() {}

// Identity is spec.holderIdentity, <kind>/<runId>/<executionId>. When it and
// the annotation disagree the holder is foreign.
func (r HolderRecord) Identity() string { return string(r.Kind) + "/" + r.RunID + "/" + r.ExecutionID }

type Reason string

const (
	ReasonHeldLive     Reason = "heldLive"
	ReasonNeedsCleanup Reason = "needsCleanup"
	ReasonCrashedRun   Reason = "crashedRun"
	// ReasonForeign is a holder this lock cannot read: holderIdentity and the
	// annotation disagree, or the annotation is unreadable.
	ReasonForeign Reason = "foreign"
)

// Refusal is why the lock was refused. Text is set for a foreign holder only.
type Refusal struct {
	Reason Reason
	Holder HolderRecord
	Text   string
}

// String shows the kind and run ID only: the execution ID is free text any
// writer of the Lease chose.
func (r Refusal) String() string {
	switch r.Reason {
	case ReasonHeldLive:
		return fmt.Sprintf("held by live %s run %s", r.Holder.Kind, r.Holder.RunID)
	case ReasonNeedsCleanup:
		return fmt.Sprintf("run %s needs cleanup first", r.Holder.RunID)
	case ReasonCrashedRun:
		return fmt.Sprintf("run %s crashed and needs recovery", r.Holder.RunID)
	}
	return "held by " + r.Text
}

type ErrorKind string

const (
	KindRefused ErrorKind = "refused"
	KindLost    ErrorKind = "lost"
	KindCluster ErrorKind = "cluster"
	// KindUnconfirmed is a release write that may have landed with no clear
	// answer, and no later try settled it: the Lease may or may not be free.
	KindUnconfirmed ErrorKind = "unconfirmed"
	KindCorrupt     ErrorKind = "corrupt"
	KindTiming      ErrorKind = "timing"
)

// Error is a lock error. Refusal is set for KindRefused, Err (a
// cluster.Error) for KindCluster and KindUnconfirmed, or the cancel of an
// unconfirmed release, Text for KindCorrupt and KindTiming.
type Error struct {
	Kind    ErrorKind
	Refusal Refusal
	Err     error
	Text    string
}

// ErrLost is the error of every write after the lock is lost, and of a
// closed, sealed or expired write gate.
var ErrLost = Error{Kind: KindLost}

func (e Error) Error() string {
	switch e.Kind {
	case KindRefused:
		return "site lock refused: " + e.Refusal.String()
	case KindLost:
		return "site lock lost"
	case KindCluster:
		return "site lock: " + e.Err.Error()
	case KindUnconfirmed:
		return "site lock release unconfirmed: " + e.Err.Error()
	case KindCorrupt:
		return "site lock record unreadable: " + e.Text
	}
	return "site lock timing invalid: " + e.Text
}

func (e Error) KindText() string { return string(e.Kind) }

func refused(r Refusal) error { return Error{Kind: KindRefused, Refusal: r} }

// Timing of the Lease: renew every 10 s, 60 s lease, writes stop when the
// last good renewal is over 30 s old, 5 s skew margin on expiry.
type Timing struct {
	RenewEvery, LeaseDuration, MaxRenewAge, ExpirySkew time.Duration
}

var DefaultTiming = Timing{10 * time.Second, 60 * time.Second, 30 * time.Second, 5 * time.Second}

// check makes renewal keep the gate open, and the gate close before the
// Lease expires for another taker.
func (t Timing) check() error {
	bad := func(text string) error { return Error{Kind: KindTiming, Text: text} }
	switch {
	case slices.ContainsFunc([]time.Duration{t.RenewEvery, t.LeaseDuration, t.MaxRenewAge, t.ExpirySkew},
		func(d time.Duration) bool { return d < 0 || d > 24*time.Hour }):
		return bad("a duration is negative or over a day")
	case t.RenewEvery == 0:
		return bad("renewEvery must be over 0")
	case t.LeaseDuration < time.Second || t.LeaseDuration%time.Second != 0:
		return bad("leaseDuration must be whole seconds, at least 1")
	case t.RenewEvery >= t.MaxRenewAge:
		return bad("renewEvery must be below maxRenewAge")
	case t.MaxRenewAge >= t.LeaseDuration:
		return bad("maxRenewAge must be below leaseDuration")
	}
	return nil
}

// HolderView is the current holder as read from the Lease. Live is renewed
// within the lease duration plus the skew. RenewAge is the wall-clock age of
// the last renewal, when known.
type HolderView struct {
	Record   HolderRecord
	Live     bool
	RenewAge *time.Duration
}

type Port interface {
	// Acquire takes the lock for me and starts renewal, which runs on its
	// own context until Release or Close.
	Acquire(ctx context.Context, me HolderRecord) (Held, error)
	// Read is the current holder, without taking anything.
	Read(ctx context.Context) (*HolderView, error)
}

type Held interface {
	Gate() *WriteGate
	// SetState refuses any state but needsCleanup once needsCleanup is
	// recorded, and writes nothing then: only an operator clears it.
	SetState(ctx context.Context, s HolderState) error
	// Release with final clears holderIdentity. A recorded needsCleanup
	// refuses it as SetState does, and the holder stays.
	Release(ctx context.Context, s HolderState) error
	// Close closes the gate and stops renewal. It leaves the Lease as it is.
	Close()
}

// CheckHolder refuses a record a Lease cannot carry, before any call.
func CheckHolder(me HolderRecord) error {
	me.State = Running
	if _, err := annotate(nil, me); err != nil {
		return err
	}
	if !plain(me.RunID) {
		return Error{Kind: KindCorrupt, Text: "the run ID has characters a Lease cannot carry"}
	}
	if !plain(me.ExecutionID) {
		return Error{Kind: KindCorrupt, Text: "the execution ID has characters a Lease cannot carry"}
	}
	return nil
}

// plain is Kubernetes name characters only, so not '/', which Identity joins on.
func plain(s string) bool {
	return len(s) > 0 && len(s) <= 300 && strings.Trim(s, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._") == ""
}

// holderOf is the holder record when the annotation is a whole record that
// agrees with identity. A foreign holder never shows its text, which anyone
// who can write the Lease chose.
func holderOf(l cluster.Lease, identity string) (HolderRecord, *Refusal) {
	var r HolderRecord
	if jsonx.Decode([]byte(l.Annotations[HolderAnnotation]), &r) != nil || r.Identity() != identity ||
		!plain(r.RunID) || !plain(r.ExecutionID) {
		return r, &Refusal{Reason: ReasonForeign, Text: "a holder this lock cannot read"}
	}
	return r, nil
}

func lastRenew(l cluster.Lease) time.Time {
	if l.RenewTime.IsZero() {
		return l.AcquireTime
	}
	return l.RenewTime
}

// live is renewed, or acquired, within the lease duration plus the skew.
func live(l cluster.Lease, now time.Time, t Timing) bool {
	last := lastRenew(l)
	return !last.IsZero() && now.Before(last.Add(time.Duration(l.LeaseDurationSeconds)*time.Second+t.ExpirySkew))
}

// decideTake says whether the Lease cur may be taken as of now: missing,
// released and an expired final holder are taken. An expired running holder
// is a crashed run, and a needsCleanup holder has unclear writes: both wait
// for an operator.
func decideTake(cur *cluster.Lease, now time.Time, t Timing) (string, *Refusal) {
	if cur == nil {
		return "missing", nil
	}
	if cur.HolderIdentity == "" {
		return "released", nil
	}
	h, r := holderOf(*cur, cur.HolderIdentity)
	isLive := live(*cur, now, t)
	switch {
	case r != nil:
		return "", r
	case h.State == Final && !isLive:
		return "expiredFinal", nil
	case h.State == NeedsCleanup:
		return "", &Refusal{Reason: ReasonNeedsCleanup, Holder: h}
	case isLive:
		return "", &Refusal{Reason: ReasonHeldLive, Holder: h}
	}
	return "", &Refusal{Reason: ReasonCrashedRun, Holder: h}
}

// WriteGate answers "may this process still write?". It closes for good
// once the lock is lost, or when the last good renewal is too old. Seal
// takes only an atomic store, so a watchdog can seal it whatever holds the
// mutex.
type WriteGate struct {
	sealed atomic.Bool
	mu     sync.Mutex
	closed bool
	until  time.Time
}

// Open is a gate that stays open until closed.
func Open() *WriteGate { return OpenUntil(time.Now().Add(1 << 62)) }

// OpenUntil is a gate open until t, moved on by ExtendUntil.
func OpenUntil(t time.Time) *WriteGate { return &WriteGate{until: t} }

// expired is closed or past the limit. Passing the limit closes the gate for
// good. The caller holds mu.
func (g *WriteGate) expired() bool {
	g.closed = g.closed || !time.Now().Before(g.until)
	return g.closed
}

// ExtendUntil follows a good renewal. It never moves the limit back, and a
// renewal that returns after the limit passed reopens nothing: expiry
// latches, whether or not anyone checked the gate in between.
func (g *WriteGate) ExtendUntil(t time.Time) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if !g.expired() && t.After(g.until) {
		g.until = t
	}
}

// CloseAt closes the gate at t at the latest.
func (g *WriteGate) CloseAt(t time.Time) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if t.Before(g.until) {
		g.until = t
	}
}

func (g *WriteGate) Close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.closed = true
}

// Seal refuses every writer but the holder's own Lease writes, which still
// stop at the renewal age.
func (g *WriteGate) Seal() { g.sealed.Store(true) }

// LeaseWritable is not closed and not past the renewal age, also when
// sealed: it gates the holder's own Lease writes only. Every other writer
// uses Check.
func (g *WriteGate) LeaseWritable() bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	return !g.expired()
}

// Check returns ErrLost when the gate is sealed, closed or past its limit.
// It reads the seal after LeaseWritable's lock, not before: a Check that
// began before a Seal still sees it, so a seal is never passed as success.
func (g *WriteGate) Check() error {
	if !g.LeaseWritable() || g.sealed.Load() {
		return ErrLost
	}
	return nil
}
