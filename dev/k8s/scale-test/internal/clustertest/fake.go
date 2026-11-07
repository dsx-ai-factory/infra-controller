// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package clustertest holds an in-memory cluster.Cluster for tests, with
// resource versions, UIDs and scripted failures.
package clustertest

import (
	"context"
	"errors"
	"maps"
	"slices"
	"strconv"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

type Op string

const (
	GetLease     Op = "GetLease"
	CreateLease  Op = "CreateLease"
	ReplaceLease Op = "ReplaceLease"
)

type Call struct {
	Op     Op
	Target string
}

// ErrNotParked is what CommitParked returns when no write is parked.
var ErrNotParked = errors.New("no write is parked")

// Fake keeps the run's namespace and its Leases.
type Fake struct {
	// Bound, when set, fails a call still running after it with Timeout, as
	// the adapter bounds its own calls.
	Bound time.Duration

	ns           string
	mu           sync.Mutex
	leases       map[string]cluster.Lease
	failures     map[Op][]error
	lost         map[Op][]error
	park         map[Op]bool
	parked       []*parked
	delays       map[Op]time.Duration
	calls        []Call
	hooks        []func(Op, string)
	version      int
	skipUID      bool
	foreignReply bool
}

// parked is a Lease write the server took but has not committed yet.
type parked struct {
	op    Op
	lease cluster.Lease
	done  chan struct{}
	out   cluster.LeaseReplaced
	err   error
}

func New(namespace string) *Fake {
	return &Fake{
		ns: namespace, leases: map[string]cluster.Lease{},
		failures: map[Op][]error{}, lost: map[Op][]error{},
		park: map[Op]bool{}, delays: map[Op]time.Duration{},
	}
}

func clone(l cluster.Lease) cluster.Lease {
	l.Annotations = maps.Clone(l.Annotations)
	return l
}

func pop(queues map[Op][]error, op Op) error {
	q := queues[op]
	if len(q) == 0 {
		return nil
	}
	queues[op] = q[1:]
	return q[0]
}

var errBound = errors.New("bound")

// do runs one call: its delay, then records it, runs the hooks and returns a
// scripted failure, else runs run under the lock. A parked write waits for
// its commit. All of it within the bound.
func (f *Fake) do(ctx context.Context, op Op, target string, run func() (*parked, error)) error {
	bound := f.Bound
	inner := ctx
	if bound > 0 {
		var cancel context.CancelFunc
		inner, cancel = context.WithTimeoutCause(ctx, bound, errBound)
		defer cancel()
	}
	err := f.enter(inner, op, target, run)
	if err != nil && ctx.Err() == nil && context.Cause(inner) == errBound {
		return cluster.Error{Kind: cluster.Timeout, Secs: int64(bound / time.Second)}
	}
	return err
}

func (f *Fake) enter(ctx context.Context, op Op, target string, run func() (*parked, error)) error {
	f.mu.Lock()
	delay := f.delays[op]
	f.mu.Unlock()
	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
		}
	}
	if ctx.Err() != nil {
		return ctx.Err()
	}
	f.mu.Lock()
	f.calls = append(f.calls, Call{op, target})
	failure, hooks := pop(f.failures, op), slices.Clone(f.hooks)
	f.mu.Unlock()
	for _, hook := range hooks {
		hook(op, target)
	}
	if failure != nil {
		return failure
	}
	f.mu.Lock()
	p, err := run()
	f.mu.Unlock()
	if p == nil {
		return err
	}
	select {
	case <-p.done:
		return p.err
	case <-ctx.Done():
		return ctx.Err()
	}
}

// store keeps l with a new resource version, and a new UID when uid is empty.
func (f *Fake) store(l cluster.Lease, uid string) cluster.Lease {
	f.version++
	l.ResourceVersion = strconv.Itoa(f.version)
	l.UID = uid
	if uid == "" {
		l.UID = "uid-" + l.ResourceVersion
	}
	f.leases[l.Name] = clone(l)
	return clone(l)
}

func (f *Fake) create(l cluster.Lease) (cluster.LeaseReplaced, error) {
	if _, ok := f.leases[l.Name]; ok {
		return cluster.LeaseReplaced{}, cluster.Error{Kind: cluster.AlreadyExists}
	}
	made := f.store(l, "")
	return cluster.LeaseReplaced{Created: true, Lease: &made}, nil
}

// replace commits as the API server does: the UID, when sent, and the
// resource version are compared when the write commits. A replace with no
// UID makes a missing Lease with a new UID, whatever version it sent, and
// answers 201.
func (f *Fake) replace(l cluster.Lease) (cluster.LeaseReplaced, error) {
	uidChecked := l.UID != "" && !f.skipUID
	cur, ok := f.leases[l.Name]
	switch {
	case !ok && uidChecked, ok && (cur.ResourceVersion != l.ResourceVersion || uidChecked && cur.UID != l.UID):
		return cluster.LeaseReplaced{}, cluster.Error{Kind: cluster.Conflict}
	case !ok:
		return f.create(l)
	}
	stored := f.store(l, cur.UID)
	if f.foreignReply {
		f.foreignReply = false
		stored.HolderIdentity = "foreign/intruder/pod-x"
	}
	return cluster.LeaseReplaced{Lease: &stored}, nil
}

// writeLease parks the write when asked, else commits it, then fails with a
// queued lost reply.
func (f *Fake) writeLease(ctx context.Context, op Op, l cluster.Lease) (cluster.LeaseReplaced, error) {
	var out cluster.LeaseReplaced
	var p *parked
	err := f.do(ctx, op, l.Name, func() (*parked, error) {
		if f.park[op] {
			delete(f.park, op)
			p = &parked{op: op, lease: clone(l), done: make(chan struct{})}
			f.parked = append(f.parked, p)
			return p, nil
		}
		var err error
		if op == CreateLease {
			out, err = f.create(l)
		} else {
			out, err = f.replace(l)
		}
		if err != nil {
			return nil, err
		}
		return nil, pop(f.lost, op)
	})
	if p != nil && err == nil {
		out = p.out
	}
	return out, err
}

func (f *Fake) GetLease(ctx context.Context, name string) (*cluster.Lease, error) {
	var out *cluster.Lease
	err := f.do(ctx, GetLease, name, func() (*parked, error) {
		if l, ok := f.leases[name]; ok {
			c := clone(l)
			out = &c
		}
		return nil, nil
	})
	return out, err
}

func (f *Fake) CreateLease(ctx context.Context, l cluster.Lease) (cluster.Lease, error) {
	r, err := f.writeLease(ctx, CreateLease, l)
	if err != nil {
		return cluster.Lease{}, err
	}
	return *r.Lease, nil
}

func (f *Fake) ReplaceLease(ctx context.Context, l cluster.Lease) (cluster.LeaseReplaced, error) {
	return f.writeLease(ctx, ReplaceLease, l)
}

// OnCall runs hook at the start of every later call, before it reads or
// writes. The hook may change this cluster.
func (f *Fake) OnCall(hook func(op Op, target string)) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.hooks = append(f.hooks, hook)
}

// Delay makes every later call of op wait d before it runs.
func (f *Fake) Delay(op Op, d time.Duration) { f.locked(func() { f.delays[op] = d }) }

// FailNext fails the next call of op with err. Queued in order.
func (f *Fake) FailNext(op Op, err error) {
	f.locked(func() { f.failures[op] = append(f.failures[op], err) })
}

// LoseReply makes the next Lease create or replace of op commit, then fail
// with err, as a write whose reply was lost.
func (f *Fake) LoseReply(op Op, err error) { f.locked(func() { f.lost[op] = append(f.lost[op], err) }) }

// ParkNext parks the next Lease create or replace of op at the server. It
// commits only on CommitParked, whether or not its caller still waits.
func (f *Fake) ParkNext(op Op) { f.locked(func() { f.park[op] = true }) }

// CommitParked commits the oldest parked write and answers its caller if it
// still waits. A caller that gave up does not undo the write.
func (f *Fake) CommitParked() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.parked) == 0 {
		return ErrNotParked
	}
	p := f.parked[0]
	f.parked = f.parked[1:]
	if p.op == CreateLease {
		p.out, p.err = f.create(p.lease)
	} else {
		p.out, p.err = f.replace(p.lease)
	}
	close(p.done)
	return p.err
}

func (f *Fake) locked(do func()) {
	f.mu.Lock()
	defer f.mu.Unlock()
	do()
}

func (f *Fake) Calls() []Call {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.calls)
}

func (f *Fake) Lease(name string) *cluster.Lease {
	f.mu.Lock()
	defer f.mu.Unlock()
	l, ok := f.leases[name]
	if !ok {
		return nil
	}
	l = clone(l)
	return &l
}

// SetLease stores l as another writer would, with a new resource version.
// It keeps the stored Lease's UID, or gets a new one when none is stored.
func (f *Fake) SetLease(l cluster.Lease) (stored cluster.Lease) {
	f.locked(func() { stored = f.store(l, f.leases[l.Name].UID) })
	return stored
}

// SkipUIDCheck makes every later Lease replace skip the UID check, as an API
// that ignores it: a replace of a missing Lease then makes it.
func (f *Fake) SkipUIDCheck() { f.locked(func() { f.skipUID = true }) }

// ForeignReply makes the next successful replace answer with a Lease that keeps
// this holder's UID but names another holder, as a server returning a wrong Lease.
func (f *Fake) ForeignReply() { f.locked(func() { f.foreignReply = true }) }

// DeleteLease deletes as an operator would, with a precondition on the
// resource version: Conflict when it changed, a 404 when it is gone.
func (f *Fake) DeleteLease(name, version string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	cur, ok := f.leases[name]
	switch {
	case !ok:
		return cluster.Error{Kind: cluster.API, Code: 404, Text: "NotFound"}
	case cur.ResourceVersion != version:
		return cluster.Error{Kind: cluster.Conflict}
	}
	delete(f.leases, name)
	return nil
}
