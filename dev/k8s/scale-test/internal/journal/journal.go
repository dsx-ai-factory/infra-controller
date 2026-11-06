// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package journal is the append-only write journal of a run,
// <dir>/<runId>.journal.jsonl: one JSON object per line, synced before the
// request it records is sent, so a write whose reply is lost is never resent
// as if nothing had landed. This package holds only the write path; recovering
// a crashed run's records from the file is out of scope for v1.
//
// A run never resumes: an existing journal for a run ID is a crashed run.
package journal

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

var (
	ErrExists      = errors.New("journal already exists: a run never resumes")
	ErrRunID       = errors.New("run ID is not a file name")
	ErrIO          = errors.New("journal I/O")
	ErrFrozen      = errors.New("journal frozen")
	ErrTooLarge    = errors.New("journal over its size cap")
	ErrLineTooLong = errors.New("journal line over its size cap")
	ErrUnclearKey  = errors.New("key has an unclear op that this write may not retry")
	ErrKeyBusy     = errors.New("key already has an op this run")
)

// MaxJournalBytes is the largest journal written. A var only so tests can
// lower it.
var MaxJournalBytes uint64 = 256 << 20

const (
	// MaxLineBytes is the longest line written.
	MaxLineBytes = 1 << 20
	// receiptRoom is what each attempt in flight keeps under the cap so its
	// receipt always fits. An Empty receipt holds no created IDs, so a fixed
	// reserve covers it.
	receiptRoom = 4 << 10
)

// Reason is why an attempt was retried or failed, as its result label: 409,
// 403-quota, 5xx, timeout, grpc-<Code> or pushback-<what>. Only the reason,
// never reply text, is kept: a reply may hold a URL or a secret.
type Reason string

const (
	Conflict       Reason = "409"
	QuotaForbidden Reason = "403-quota"
	ServerError    Reason = "5xx"
	Timeout        Reason = "timeout"
)

func Grpc(code string) Reason     { return Reason("grpc-" + code) }
func Pushback(what string) Reason { return Reason("pushback-" + what) }

// WriteCall is one logical write. Attempts of one op share its Key.
type WriteCall struct {
	Scope         string // who sends it, e.g. test
	Kind          string // the write kind, e.g. expectedMachines
	Key           string
	Idempotent    bool // safe to resend after a lost reply
	Hosts         []string
	RequestDigest string
}

// Header is the first entry of every journal. Create sets Entry.
type Header struct {
	Entry   string `json:"entry"` // header
	RunID   string `json:"runId"`
	Command string `json:"command"`
}

// Intent is written and synced before its attempt is dispatched.
type Intent struct {
	Entry         string     `json:"entry"` // intent
	Seq           uint64     `json:"seq"`
	Op            uint64     `json:"op"`
	Attempt       uint32     `json:"attempt"`
	Key           string     `json:"key"`
	Kind          string     `json:"kind"`
	Idempotent    bool       `json:"idempotent"`
	Hosts         []string   `json:"hosts"`
	RequestDigest string     `json:"requestDigest"`
	At            jsonx.Time `json:"at"`
}

// Receipt is the reply, or the non-dispatch, of one attempt.
type Receipt struct {
	Entry   string     `json:"entry"` // receipt
	Seq     uint64     `json:"seq"`
	Op      uint64     `json:"op"`
	Outcome Outcome    `json:"outcome"`
	At      jsonx.Time `json:"at"`
}

// Outcome of one attempt. A NICo write returns Empty, so an accepted attempt
// creates no IDs: Kind alone, or a rejection's Reason, is kept.
type Outcome struct {
	Kind   string `json:"kind"`
	Reason Reason `json:"reason,omitempty"`
}

const (
	Accepted      = "accepted"
	Rejected      = "rejected"
	NotDispatched = "notDispatched"
	// Planned: a dry run journaled the write and sent nothing. It settles the
	// op like a clean non-dispatch and is never a server acceptance.
	Planned = "planned"
)

// IntentRef is the persisted intent of one attempt, for its receipt.
type IntentRef struct {
	Op, Seq uint64
	Attempt uint32
}

type status uint8

const (
	inFlight     status = iota // intent written, no receipt yet
	retryPending               // rejected, the writer may retry
	unclear                    // no reply and no accepted attempt
)

type track struct {
	op         uint64
	attempts   uint32
	idempotent bool
	digest     string
	status     status
	lostReply  bool // an earlier attempt got no reply
}

// Journal is one run's journal, shared by every writer. One mutex covers op
// numbering, the open ops and each append: one whole line, then a sync. The
// first append or sync that fails breaks the journal for good, so a line after
// a torn one never corrupts the records. An entry refused for a size cap
// writes nothing and breaks nothing.
type Journal struct {
	path   string
	frozen atomic.Bool
	keysMu sync.Mutex
	keys   map[string]bool // keys with an op this run, over every writer

	// AfterSync runs after each synced append, outside the lock. Tests set
	// it before any concurrent use.
	AfterSync func(any)

	mu              sync.Mutex
	file            *os.File
	fsync           func(*os.File) error // (*os.File).Sync; a seam for tests
	nextSeq, nextOp uint64
	open            map[string]*track
	broken          error
	attempts        uint64 // intents, less those settled notDispatched or planned
	bytes, max      uint64
	reserved        uint64
	fault           func(*os.File, []byte) error
}

func pathFor(dir, run string) (string, error) {
	if run == "" || filepath.Base(run) != run {
		return "", ErrRunID
	}
	return filepath.Join(dir, run+".journal.jsonl"), nil
}

func ioErr(err error) error { return fmt.Errorf("%w: %v", ErrIO, err) }

// Create makes the journal of a new run and writes its header. It fails when
// the file exists: a run never resumes.
func Create(dir string, h Header) (*Journal, error) {
	path, err := pathFor(dir, h.RunID)
	if err != nil {
		return nil, err
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE|os.O_EXCL, 0o600)
	if errors.Is(err, fs.ErrExist) {
		return nil, fmt.Errorf("%w: %s", ErrExists, path)
	} else if err != nil {
		return nil, ioErr(err)
	}
	h.Entry = "header"
	j := &Journal{path: path, keys: map[string]bool{}, file: f, fsync: (*os.File).Sync,
		nextSeq: 1, nextOp: 1, open: map[string]*track{}, max: MaxJournalBytes}
	// The new directory entry must survive a crash too.
	if err = syncDir(dir); err == nil {
		err = j.append(h)
	}
	if err != nil {
		_ = f.Close()
		return nil, err
	}
	return j, nil
}

func syncDir(dir string) error {
	d, err := os.Open(dir)
	if err == nil {
		err = errors.Join(d.Sync(), d.Close())
	}
	if err != nil {
		return ioErr(err)
	}
	return nil
}

func (j *Journal) Path() string { return j.path }

// Freeze refuses every later append. It never waits, so a drain can freeze a
// journal whose append is stuck.
func (j *Journal) Freeze() { j.frozen.Store(true) }

// Unsettled is true when any op this run lacks a proven clean outcome (one
// stays open) or a journal write has failed. A site released while Unsettled
// must stay needsCleanup: a write it recorded may still land on the fleet.
func (j *Journal) Unsettled() bool {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.broken != nil || len(j.open) > 0
}

// Counts is a snapshot of the journal's records, from what is already tracked.
// Ops is every op with a persisted intent, planned or sent. Attempts is the
// persisted attempt records that were not settled notDispatched or planned, so
// it is the RPCs that went out or may have (an attempt in flight, or a lost
// reply, counts): a dry run has Ops and no Attempts. Unclear is the ops with
// no proven outcome: an attempt in flight, a lost reply or an unclear receipt.
// Known is false once a journal write has failed: a torn line may hide a
// record, so no count is trusted.
type Counts struct {
	Ops, Attempts, Unclear uint64
	Known                  bool
}

// Counts reads the counts under the journal's lock. It writes and replays
// nothing.
func (j *Journal) Counts() Counts {
	j.mu.Lock()
	defer j.mu.Unlock()
	c := Counts{Ops: j.nextOp - 1, Attempts: j.attempts, Known: j.broken == nil}
	for _, t := range j.open {
		if t.status != retryPending || t.lostReply {
			c.Unclear++
		}
	}
	return c
}

// Close freezes the journal and closes its file. Unlike Freeze, it waits for
// an append in progress.
func (j *Journal) Close() error {
	j.Freeze()
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.file.Close()
}

// Inject makes the next append run fault before its own write, so a test can
// tear a line or fail a sync.
func (j *Journal) Inject(fault func(f *os.File, line []byte) error) {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.fault = fault
}

// Cap lowers the size cap, for tests.
func (j *Journal) Cap(max uint64) {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.max = max
}

// locked runs f under the lock, then the AfterSync hook on the entry f
// wrote, if any.
func (j *Journal) locked(f func() (any, error)) error {
	j.mu.Lock()
	e, err := f()
	j.mu.Unlock()
	if e != nil && err == nil && j.AfterSync != nil {
		j.AfterSync(e)
	}
	return err
}

func (j *Journal) append(e any) error {
	return j.locked(func() (any, error) { return e, j.write(e, j.reserved) })
}

// write appends e if keep bytes still fit under the cap after it: the room
// the attempts in flight keep for their receipts. The lock is held.
func (j *Journal) write(e any, keep uint64) error {
	if j.frozen.Load() {
		return ErrFrozen
	}
	if j.broken != nil {
		return fmt.Errorf("%w: an earlier append failed, so no later one is safe: %v", ErrIO, j.broken)
	}
	line, err := jsonx.Encode(e)
	if err != nil {
		return ioErr(err)
	}
	// Nothing is written that a recovery read could not parse.
	if len(line) > MaxLineBytes {
		return ErrLineTooLong
	}
	line = append(line, '\n')
	if after := j.bytes + uint64(len(line)); after+keep > j.max {
		return ErrTooLarge
	}
	// Neither retried nor cut back: a failed sync may have dropped what it
	// wrote, and another line after a torn one would corrupt the records.
	if err := j.put(line); err != nil {
		j.broken = err
		return ioErr(err)
	}
	j.bytes += uint64(len(line))
	return nil
}

func (j *Journal) put(line []byte) error {
	if fault := j.fault; fault != nil {
		j.fault = nil
		if err := fault(j.file, line); err != nil {
			return err
		}
	}
	if _, err := j.file.Write(line); err != nil {
		return err
	}
	return j.fsync(j.file)
}

// Claim reserves key for the run: one logical op per key, over every writer.
// A second claim of a key is refused. There is no release; the key stays used
// until the run ends, so a used key never starts a second op.
func (j *Journal) Claim(key string) error {
	j.keysMu.Lock()
	defer j.keysMu.Unlock()
	if j.keys[key] {
		return fmt.Errorf("%w: %s", ErrKeyBusy, key)
	}
	j.keys[key] = true
	return nil
}

// Intent persists the intent of one attempt. It starts the key's op on the
// first attempt, or adds an attempt to it. Once any attempt of the op lost its
// reply, a new attempt is allowed only for an idempotent call with the same
// request digest, also while a later rejection waits for its retry. An attempt
// in flight excludes any other.
func (j *Journal) Intent(c WriteCall) (IntentRef, error) {
	at := jsonx.Time{Time: time.Now().UTC()}
	var ref IntentRef
	err := j.locked(func() (any, error) {
		t := j.open[c.Key]
		ref.Op, ref.Attempt = j.nextOp, 1
		if t != nil {
			if t.status == inFlight {
				return nil, fmt.Errorf("%w: %s", ErrKeyBusy, c.Key)
			}
			same := c.Idempotent && t.idempotent && t.digest == c.RequestDigest
			if !same && (t.status == unclear || t.lostReply) {
				return nil, fmt.Errorf("%w: %s", ErrUnclearKey, c.Key)
			}
			ref.Op, ref.Attempt = t.op, t.attempts+1
		}
		ref.Seq = j.nextSeq
		e := Intent{"intent", ref.Seq, ref.Op, ref.Attempt, c.Key, c.Kind, c.Idempotent, append([]string{}, c.Hosts...), c.RequestDigest, at}
		if err := j.write(e, j.reserved+receiptRoom); err != nil {
			return nil, err
		}
		j.nextSeq++
		j.attempts++
		if ref.Attempt == 1 {
			j.nextOp++
		}
		next := &track{op: ref.Op, attempts: ref.Attempt, idempotent: c.Idempotent, digest: c.RequestDigest}
		if t != nil {
			next.lostReply = t.lostReply
		}
		j.open[c.Key] = next
		j.reserved += receiptRoom
		return e, nil
	})
	return ref, err
}

// Receipt persists the reply, or the non-dispatch, of one attempt. If it
// fails, the attempt reads as a lost reply: an accepted op is never resent as
// if nothing had landed.
func (j *Journal) Receipt(in IntentRef, o Outcome) error {
	e := Receipt{"receipt", in.Seq, in.Op, o, jsonx.Time{Time: time.Now().UTC()}}
	return j.locked(func() (any, error) {
		key, t := j.openOp(in.Op)
		var own uint64 // a receipt uses the room its attempt kept
		if t != nil && t.status == inFlight {
			own = receiptRoom
		}
		err := j.write(e, j.reserved-own)
		if t == nil {
			return e, err
		}
		j.land(t)
		if err == nil && (o.Kind == NotDispatched || o.Kind == Planned) {
			j.attempts--
		}
		switch {
		case err != nil:
			t.lostReply = true
		case o.Kind == Accepted, (o.Kind == NotDispatched || o.Kind == Planned) && !t.lostReply:
			delete(j.open, key)
		case o.Kind == Rejected:
			t.status = retryPending
		default:
			t.status = unclear
		}
		return e, err
	})
}

func (j *Journal) openOp(op uint64) (string, *track) {
	for k, t := range j.open {
		if t.op == op {
			return k, t
		}
	}
	return "", nil
}

// land ends the attempt in flight on t: its receipt room is free. The caller
// sets t's next status.
func (j *Journal) land(t *track) {
	if t.status == inFlight {
		t.status = unclear
		j.reserved -= receiptRoom
	}
}

// MarkUnclear records that the attempt of op got no reply, or one that may
// have come after the write landed. Nothing is written: an intent without a
// receipt already reads as unclear.
func (j *Journal) MarkUnclear(op uint64) {
	j.mu.Lock()
	defer j.mu.Unlock()
	if _, t := j.openOp(op); t != nil {
		j.land(t)
		t.lostReply = true
	}
}

// LostReply is true when an attempt of the open op, in any attempt, lost its
// reply.
func (j *Journal) LostReply(op uint64) bool {
	j.mu.Lock()
	defer j.mu.Unlock()
	_, t := j.openOp(op)
	return t != nil && t.lostReply
}

// CloseFailed ends op after a rejection. An op where any attempt lost its
// reply stays unclear: true then.
func (j *Journal) CloseFailed(op uint64) bool {
	j.mu.Lock()
	defer j.mu.Unlock()
	key, t := j.openOp(op)
	if t == nil {
		return false
	}
	j.land(t)
	if !t.lostReply {
		delete(j.open, key)
	}
	return t.lostReply
}
