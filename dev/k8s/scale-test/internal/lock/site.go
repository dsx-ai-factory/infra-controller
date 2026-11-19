// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package lock

import (
	"context"
	"errors"
	"log/slog"
	"maps"
	"reflect"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

const (
	releaseTries = 3           // most writes one release makes
	releasePause = time.Second // the wait before a release is tried again
)

// SiteLock is the Port on one Lease per site in the run's namespace. Cluster
// bounds every call itself: a second timeout here would race the adapter's
// own and could turn a refusal into a timeout.
type SiteLock struct {
	Cluster cluster.Cluster
	Name    string
	Timing  Timing
}

// clusterErr wraps a cluster.Error. Any other error, a cancel, stays as it is.
func clusterErr(err error) error {
	var e cluster.Error
	if errors.As(err, &e) {
		return Error{Kind: KindCluster, Err: e}
	}
	return err
}

// annotate is a copy of a with r in it. A record that does not encode, such
// as one of a kind or state the lock does not know, is refused.
func annotate(a map[string]string, r HolderRecord) (map[string]string, error) {
	raw, err := jsonx.Encode(r)
	if err != nil {
		return nil, Error{Kind: KindCorrupt, Text: "the holder record does not encode"}
	}
	a = maps.Collect(maps.All(a))
	a[HolderAnnotation] = string(raw)
	return a, nil
}

func (l *SiteLock) Acquire(ctx context.Context, me HolderRecord) (Held, error) {
	// Before any write: a bad timing would break renewal after the Lease is
	// taken, and a record the Lease cannot carry would never read back.
	if err := l.Timing.check(); err != nil {
		return nil, err
	}
	if err := CheckHolder(me); err != nil {
		return nil, err
	}
	cur, err := l.Cluster.GetLease(ctx, l.Name)
	if err != nil {
		return nil, clusterErr(err)
	}
	// The Lease times and the gate start from the same moment, after the
	// read: the gate must close before the Lease these times write ends.
	now := time.Now()
	if _, r := decideTake(cur, now, l.Timing); r != nil {
		return nil, refused(*r)
	}
	me.State = Running
	token := uuid.NewString()
	lease := cluster.Lease{
		Name: l.Name, HolderIdentity: me.Identity(), LeaseDurationSeconds: int32(l.Timing.LeaseDuration / time.Second),
		AcquireTime: now.UTC().Round(0), RenewTime: now.UTC().Round(0),
	}
	if cur != nil {
		lease.ResourceVersion, lease.UID, lease.Annotations = cur.ResourceVersion, cur.UID, cur.Annotations
	}
	if lease.Annotations, err = annotate(lease.Annotations, me); err != nil {
		return nil, err
	}
	lease.Annotations[TokenAnnotation] = token
	var stored cluster.Lease
	if cur == nil {
		stored, err = l.Cluster.CreateLease(ctx, lease)
	} else {
		// Only acquire may make the Lease: a replace that made it again is
		// this attempt's write all the same. A 201 with no Lease in it is
		// read back as a lost reply.
		var r cluster.LeaseReplaced
		if r, err = l.Cluster.ReplaceLease(ctx, lease); err == nil && r.Lease == nil {
			err = cluster.Error{Kind: cluster.Invalid, Text: "the reply did not decode"}
		} else if err == nil {
			stored = *r.Lease
		}
	}
	var ce cluster.Error
	if errors.As(err, &ce) && (ce.Kind == cluster.AlreadyExists || ce.Kind == cluster.Conflict) {
		// Another taker wrote first, whether or not the read after it works:
		// the site is busy, not the API down.
		refusal := &Refusal{Reason: ReasonForeign, Text: "a concurrent taker"}
		if c, rerr := l.Cluster.GetLease(ctx, l.Name); rerr == nil && c != nil {
			if _, r := decideTake(c, time.Now(), l.Timing); r != nil {
				refusal = r
			}
		}
		return nil, refused(*refusal)
	}
	if err != nil {
		// The write may have committed with its reply lost: only this
		// attempt's token proves it. Another holder seen instead refuses.
		c, rerr := l.Cluster.GetLease(ctx, l.Name)
		if rerr != nil || c == nil {
			return nil, clusterErr(err)
		}
		if h, r := holderOf(*c, me.Identity()); r != nil || c.Annotations[TokenAnnotation] != token ||
			c.HolderIdentity != me.Identity() || !reflect.DeepEqual(h, me) {
			if _, r := decideTake(c, time.Now(), l.Timing); r != nil {
				return nil, refused(*r)
			}
			return nil, clusterErr(err)
		}
		stored = *c
	}
	// Renewals cannot tell this Lease from one made again without its UID.
	if stored.UID == "" {
		return nil, clusterErr(cluster.Error{Kind: cluster.Invalid, Text: "the Lease has no UID"})
	}
	rctx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	h := &held{
		lock: l, record: me, token: token, uid: stored.UID, gate: OpenUntil(now.Add(l.Timing.MaxRenewAge)),
		sem: make(chan struct{}, 1), lease: stored, ctx: rctx, cancel: cancel, done: make(chan struct{}),
	}
	go h.renew()
	return h, nil
}

func (l *SiteLock) Read(ctx context.Context) (*HolderView, error) {
	cur, err := l.Cluster.GetLease(ctx, l.Name)
	if err != nil || cur == nil || cur.HolderIdentity == "" {
		return nil, clusterErr(err)
	}
	r, refusal := holderOf(*cur, cur.HolderIdentity)
	if refusal != nil {
		return nil, refused(*refusal)
	}
	now := time.Now()
	v := &HolderView{Record: r, Live: live(*cur, now, l.Timing)}
	if last := lastRenew(*cur); !last.IsZero() && !now.Before(last) {
		age := now.Sub(last)
		v.RenewAge = &age
	}
	return v, nil
}

type held struct {
	lock   *SiteLock
	record HolderRecord
	// token and uid are the acquire's: every later write needs the Lease to
	// still carry both.
	token, uid string
	gate       *WriteGate
	// sem is the mutex of renewal, SetState and Release, waited on with a
	// context. It guards lease and lost.
	sem   chan struct{}
	lease cluster.Lease
	// lost is set once another holder is seen or the gate closed. Never
	// cleared.
	lost bool
	// ctx is renewal's: it ends only at Release, Close or a loss.
	ctx    context.Context
	cancel context.CancelFunc
	done   chan struct{}
}

// change is how an update changes the cached Lease: a renewal, or a state
// set by SetState or by Release.
type change struct {
	renew, release bool
	state          HolderState
}

func (h *held) acquire(ctx context.Context) error {
	select {
	case h.sem <- struct{}{}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (h *held) unlock() { <-h.sem }

// renew ticks every RenewEvery. Past the renewal age it gives up, else a
// good renewal moves the gate on from when it was sent.
func (h *held) renew() {
	defer close(h.done)
	tick := time.NewTicker(h.lock.Timing.RenewEvery)
	defer tick.Stop()
	for {
		select {
		case <-h.ctx.Done():
			return
		case <-tick.C:
		}
		if !h.gate.LeaseWritable() {
			h.giveUp()
			return
		}
		sent := time.Now()
		_, err := h.update(h.ctx, change{renew: true})
		switch {
		case h.ctx.Err() != nil || err == ErrLost:
			return
		case err == nil:
			h.gate.ExtendUntil(sent.Add(h.lock.Timing.MaxRenewAge))
		default:
			slog.Warn("site lock renewal failed", "error", err.Error())
		}
	}
}

// giveUp follows renewals that failed for the renewal age: the run stops
// writing, and one write on the cached version marks needsCleanup. It is
// never retried, so a Lease another process took is never written.
func (h *held) giveUp() {
	if h.acquire(h.ctx) != nil {
		return
	}
	defer h.unlock()
	// acquire can win the free semaphore over a done context, so re-check it:
	// Close cancels before closing the gate, and a tick that races Close must
	// not stamp needsCleanup on a Lease left for normal recovery.
	if h.ctx.Err() != nil || h.lost {
		return
	}
	h.lost = true
	h.gate.Close()
	slog.Error("site lock renewals failed too long, the lock is lost")
	next, err := h.changed(h.lease, change{state: NeedsCleanup})
	if err != nil {
		return
	}
	r, err := h.lock.Cluster.ReplaceLease(h.ctx, next)
	switch {
	case err != nil:
		slog.Warn("needsCleanup not recorded on the Lease", "error", err.Error())
	case h.ours(r):
		h.lease = *r.Lease
	}
}

// ours is a replace that wrote the Lease acquire took. Any other reply made
// a Lease, or wrote one with another UID: the one taken was deleted.
func (h *held) ours(r cluster.LeaseReplaced) bool { return !r.Created && r.Lease.UID == h.uid }

// recordOn is the record l carries for this holder, when l is the Lease
// acquire took with this acquire's token and this execution in it.
func (h *held) recordOn(l cluster.Lease) (HolderRecord, bool) {
	r, refusal := holderOf(l, h.record.Identity())
	return r, refusal == nil && l.UID == h.uid && l.Annotations[TokenAnnotation] == h.token && r.ExecutionID == h.record.ExecutionID
}

// namesMe is the Lease acquire took, still naming this exact holder. A Lease
// replaced by another acquire of the same record has another token, and one
// deleted and made again another UID: neither is ours.
func (h *held) namesMe(l cluster.Lease) bool {
	_, ok := h.recordOn(l)
	return ok && l.HolderIdentity == h.record.Identity()
}

func (h *held) changed(l cluster.Lease, c change) (cluster.Lease, error) {
	r, refusal := holderOf(l, h.record.Identity())
	if refusal != nil {
		return l, refused(*refusal)
	}
	switch {
	case c.renew:
		l.RenewTime = time.Now().UTC().Round(0)
	// A recorded needsCleanup is never settled by its own holder.
	case r.State == NeedsCleanup && c.state != NeedsCleanup:
		return l, refused(Refusal{Reason: ReasonNeedsCleanup, Holder: r})
	default:
		r.State = c.state
		if c.release && c.state == Final {
			l.HolderIdentity = ""
		}
	}
	var err error
	l.Annotations, err = annotate(l.Annotations, r)
	return l, err
}

// update is one compare-and-swap write, tried once more after a conflict
// when the Lease still names this holder. wrote says the error came from a
// Lease write, which may have landed.
func (h *held) update(ctx context.Context, c change) (wrote bool, err error) {
	if err := h.acquire(ctx); err != nil {
		return false, err
	}
	defer h.unlock()
	for range 2 {
		// Past the renewal age only giveUp may write, once.
		if h.lost || !h.gate.LeaseWritable() {
			return false, ErrLost
		}
		if !h.namesMe(h.lease) {
			return false, h.lose()
		}
		next, err := h.changed(h.lease, c)
		if err != nil {
			return false, err
		}
		r, err := h.lock.Cluster.ReplaceLease(ctx, next)
		var ce cluster.Error
		errors.As(err, &ce)
		switch {
		case err == nil && h.ours(r):
			h.lease = *r.Lease
			return false, nil
		case err == nil:
			// The Lease was deleted and this write made it again, which only
			// an API skipping the UID check allows. The lock is lost and the
			// stray Lease is left for an operator.
			return false, h.lose()
		case ce.Kind == cluster.API && ce.Code == 404:
			return false, h.lose()
		case ce.Kind != cluster.Conflict:
			return true, clusterErr(err)
		}
		cur, err := h.lock.Cluster.GetLease(ctx, h.lease.Name)
		switch {
		case err != nil:
			return false, clusterErr(err)
		case cur == nil:
			return false, h.lose()
		case h.namesMe(*cur):
			h.lease = *cur
			continue
		}
		// An earlier try of this release committed with its reply lost.
		if r, ok := h.recordOn(*cur); ok && c.release && cur.HolderIdentity == "" && r.State == c.state {
			h.lease = *cur
			return false, nil
		}
		return false, h.lose()
	}
	// The last read back may carry a needsCleanup that refuses this change.
	if _, err := h.changed(h.lease, c); err != nil {
		return false, err
	}
	return false, clusterErr(cluster.Error{Kind: cluster.Conflict})
}

func (h *held) lose() error {
	h.lost = true
	h.gate.Close()
	h.cancel()
	slog.Error("the Lease no longer names this holder, the site lock is lost")
	return ErrLost
}

func (h *held) Gate() *WriteGate { return h.gate }

func (h *held) SetState(ctx context.Context, s HolderState) error {
	_, err := h.update(ctx, change{state: s})
	return err
}

// Release seals the gate first: once the Lease is free another run can take
// it before this reply arrives. A transient failure is tried again while the
// Lease may still be written, since an unreleased Lease waits for an
// operator once it expires.
func (h *held) Release(ctx context.Context, s HolderState) error {
	h.gate.Seal()
	h.cancel()
	<-h.done
	wrote, err := h.update(ctx, change{release: true, state: s})
	// A write whose reply never came may have released the Lease, so a later
	// loss to another holder is no proof the release failed. The first such
	// write decides: a read, or a definite refusal such as a 429, never
	// landed.
	var unconfirmed error
	for tried := 1; err != nil; tried++ {
		var le Error
		if !errors.As(err, &le) || le.Kind != KindCluster {
			// A sent write that a cancel cut short may have landed too.
			if wrote && unconfirmed == nil {
				unconfirmed = err
			}
			break
		}
		ce := le.Err.(cluster.Error)
		if wrote && unconfirmed == nil && maybeCommitted(ce) {
			unconfirmed = ce
		}
		if tried == releaseTries || !transient(ce) || !h.gate.LeaseWritable() {
			break
		}
		select {
		case <-time.After(releasePause):
		case <-ctx.Done():
		}
		if ctx.Err() != nil || !h.gate.LeaseWritable() {
			break
		}
		wrote, err = h.update(ctx, change{release: true, state: s})
	}
	h.gate.Close()
	var le Error
	switch {
	case err == nil, errors.As(err, &le) && le.Kind == KindRefused:
		return err
	case unconfirmed != nil:
		return Error{Kind: KindUnconfirmed, Err: unconfirmed}
	}
	return err
}

// Close stops renewal and closes the gate, in that order: cancelling first
// means a tick the renew goroutine still takes finds the context done, so it
// never gives up and stamps needsCleanup. The Lease is left for recovery.
func (h *held) Close() {
	h.cancel()
	h.gate.Close()
	<-h.done
}

// maybeCommitted is a failure after which the write may have landed: no
// answer, an answer that is not valid, or any server error. A 503 is one
// too: a proxy answers 503 when the server went away after the request
// reached it.
func maybeCommitted(e cluster.Error) bool {
	return e.Kind == cluster.Timeout || e.Kind == cluster.Transport || e.Kind == cluster.Invalid ||
		e.Kind == cluster.API && e.Code >= 500
}

// transient is a failure the same call may succeed after.
func transient(e cluster.Error) bool {
	return e.Kind == cluster.Timeout || e.Kind == cluster.Transport ||
		e.Kind == cluster.API && (e.Code == 408 || e.Code == 429 || e.Code >= 500)
}
