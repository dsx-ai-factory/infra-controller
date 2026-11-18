// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package journal

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"
)

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

// ok is must for goroutines other than the test's own.
func ok(t *testing.T, err error) {
	if err != nil {
		t.Error(err)
	}
}

func is(t *testing.T, err, want error) {
	t.Helper()
	if !errors.Is(err, want) {
		t.Fatalf("got %v, want %v", err, want)
	}
}

func eq[T comparable](t *testing.T, got, want T) {
	t.Helper()
	if got != want {
		t.Fatalf("got %+v, want %+v", got, want)
	}
}

func call(key, digest string) WriteCall {
	return WriteCall{Scope: "test", Kind: "batch", Key: key, Idempotent: true, Hosts: []string{"fm100h1"}, RequestDigest: digest}
}

func size(t *testing.T, j *Journal) uint64 {
	st, err := os.Stat(j.Path())
	must(t, err)
	return uint64(st.Size())
}

// lines reads the journal file back as raw entries, one map per line. A torn
// or interleaved line fails to decode, so this also proves every line is
// whole. (The recovery reader is a later PR; tests read raw.)
func lines(t *testing.T, j *Journal) []map[string]any {
	t.Helper()
	data, err := os.ReadFile(j.Path())
	must(t, err)
	var out []map[string]any
	for _, ln := range strings.Split(strings.TrimRight(string(data), "\n"), "\n") {
		if ln == "" {
			continue
		}
		var m map[string]any
		must(t, json.Unmarshal([]byte(ln), &m))
		out = append(out, m)
	}
	return out
}

// tags lists the entries on disk by tag, an intent as intentN for its
// attempt, a receipt by its outcome kind.
func tags(t *testing.T, j *Journal) string {
	var s []string
	for _, m := range lines(t, j) {
		switch m["entry"] {
		case "intent":
			s = append(s, fmt.Sprintf("intent%v", m["attempt"]))
		case "receipt":
			s = append(s, "receipt-"+m["outcome"].(map[string]any)["kind"].(string))
		default:
			s = append(s, m["entry"].(string))
		}
	}
	return strings.Join(s, " ")
}

func failSync(f *os.File, line []byte) error {
	_, _ = f.Write(line) // a torn tail: bytes on disk, sync failed
	return errors.New("injected: sync failed")
}

func TestJournal(t *testing.T) {
	for _, row := range []struct {
		name string
		run  func(t *testing.T, dir string, j *Journal)
	}{
		{"create_refuses_an_existing_file_or_a_bad_run_id", func(t *testing.T, dir string, j *Journal) {
			_, err := Create(dir, Header{RunID: "r1", Command: "burst"})
			is(t, err, ErrExists)
			eq(t, tags(t, j), "header")
			j2, err := Create(dir, Header{RunID: "r2", Command: "burst"})
			must(t, err)
			must(t, j2.Close())
			_, err = Create(dir, Header{RunID: "../r3"})
			is(t, err, ErrRunID)
		}},
		{"a_partial_intent_write_breaks_the_journal", func(t *testing.T, _ string, j *Journal) {
			j.Inject(func(f *os.File, line []byte) error { _, _ = f.Write(line[:3]); return errors.New("injected: torn") })
			_, err := j.Intent(call("k1", "d1"))
			is(t, err, ErrIO)
			// Every later append fails with no I/O: a line after a torn one
			// would break replay.
			_, err = j.Intent(call("k2", "d2"))
			is(t, err, ErrIO)
			is(t, j.Receipt(IntentRef{Op: 9, Seq: 9}, Outcome{Kind: Accepted}), ErrIO)
		}},
		{"a_receipt_sync_failure_breaks_the_journal", func(t *testing.T, _ string, j *Journal) {
			in, err := j.Intent(call("k1", "d1"))
			must(t, err)
			j.Inject(failSync)
			is(t, j.Receipt(in, Outcome{Kind: Accepted}), ErrIO)
			_, err = j.Intent(call("k2", "d2"))
			is(t, err, ErrIO)
		}},
		{"a_line_over_the_cap_is_never_written_and_does_not_break", func(t *testing.T, _ string, j *Journal) {
			before := size(t, j)
			c := call("k1", "d1")
			c.Hosts = []string{strings.Repeat("h", MaxLineBytes)}
			is(t, func() error { _, e := j.Intent(c); return e }(), ErrLineTooLong)
			eq(t, size(t, j), before)
			_, err := j.Intent(call("k2", "d2")) // not broken
			must(t, err)
		}},
		{"a_refused_intent_leaves_the_journal_whole", func(t *testing.T, _ string, j *Journal) {
			first, err := j.Intent(call("k1", "d1"))
			must(t, err)
			j.Cap(size(t, j) + receiptRoom + 100) // room for the first receipt only
			_, err = j.Intent(call("k2", "d2"))
			is(t, err, ErrTooLarge)
			must(t, j.Receipt(first, Outcome{Kind: Accepted}))
			eq(t, tags(t, j), "header intent1 receipt-accepted")
		}},
		{"each_intent_in_flight_keeps_its_own_receipt_room", func(t *testing.T, _ string, j *Journal) {
			before := size(t, j)
			_, err := j.Intent(call("k1", "d1"))
			must(t, err)
			line, room := size(t, j)-before, uint64(receiptRoom)
			j.Cap(before + 2*line + room*3/2) // two intents, one receipt and a half
			_, err = j.Intent(call("k2", "d2"))
			is(t, err, ErrTooLarge)
			// Once the first lands, the second fits, and its receipt frees its
			// room for the third.
			j.MarkUnclear(1)
			second, err := j.Intent(call("k2", "d2"))
			must(t, err)
			must(t, j.Receipt(second, Outcome{Kind: Rejected, Reason: Conflict}))
			j.Cap(size(t, j) + line + room)
			_, err = j.Intent(call("k3", "d3"))
			must(t, err)
		}},
		{"ten_thousand_attempts_in_flight_keep_under_a_fifth_of_the_cap", func(t *testing.T, _ string, _ *Journal) {
			if 10_000*receiptRoom >= MaxJournalBytes/5 {
				t.Fatal("receipt room too large")
			}
		}},
		{"a_claimed_or_in_flight_key_refuses_a_second_op", func(t *testing.T, _ string, j *Journal) {
			_, err := j.Intent(call("k1", "d1")) // in flight, no receipt yet
			must(t, err)
			_, err = j.Intent(call("k1", "d1"))
			is(t, err, ErrKeyBusy)
			must(t, j.Claim("k2")) // one op per key for the run: no release
			is(t, j.Claim("k2"), ErrKeyBusy)
		}},
		{"a_lost_reply_refuses_a_different_payload_but_keeps_the_same_one", func(t *testing.T, _ string, j *Journal) {
			in, err := j.Intent(call("k1", "d1"))
			must(t, err)
			j.MarkUnclear(in.Op)
			eq(t, j.LostReply(in.Op), true)
			_, err = j.Intent(call("k1", "d2")) // another payload on an unclear op
			is(t, err, ErrUnclearKey)
			again, err := j.Intent(call("k1", "d1")) // same idempotent payload continues the op
			must(t, err)
			eq(t, [2]uint64{again.Op, uint64(again.Attempt)}, [2]uint64{in.Op, 2})
		}},
		{"close_failed_keeps_a_lost_reply_unclear", func(t *testing.T, _ string, j *Journal) {
			in, err := j.Intent(call("k1", "d1"))
			must(t, err)
			must(t, j.Receipt(in, Outcome{Kind: Rejected, Reason: Conflict}))
			eq(t, j.CloseFailed(in.Op), false) // a clean rejection: not unclear
			in2, err := j.Intent(call("k2", "d2"))
			must(t, err)
			j.MarkUnclear(in2.Op)
			eq(t, j.CloseFailed(in2.Op), true) // an attempt lost its reply
		}},
	} {
		t.Run(row.name, func(t *testing.T) {
			dir := t.TempDir()
			j, err := Create(dir, Header{RunID: "r1", Command: "burst"})
			must(t, err)
			defer func() { _ = j.Close() }()
			row.run(t, dir, j)
		})
	}
}

// TestEachAppendFsyncsBeforeTheCallback proves every append calls fsync, and
// that the sync happens before AfterSync observes the entry. It fails if the
// Sync call in put is skipped.
func TestEachAppendFsyncsBeforeTheCallback(t *testing.T) {
	dir := t.TempDir()
	j, err := Create(dir, Header{RunID: "r1", Command: "burst"})
	must(t, err)
	defer func() { _ = j.Close() }()
	var syncs atomic.Int64
	real := j.fsync
	j.fsync = func(f *os.File) error { syncs.Add(1); return real(f) } // the records still land
	syncedBeforeCallback := true
	j.AfterSync = func(any) {
		if syncs.Load() == 0 {
			syncedBeforeCallback = false
		}
	}
	before := syncs.Load() // the header synced inside Create, before the wrap
	in, err := j.Intent(call("k1", "d1"))
	must(t, err)
	must(t, j.Receipt(in, Outcome{Kind: Accepted}))
	eq(t, syncs.Load()-before, int64(2)) // one sync per append: the intent and its receipt
	eq(t, syncedBeforeCallback, true)
}

// TestConcurrentAppendsAreWhole checks that intents from many writers never
// interleave or reorder: every line decodes and the count is exact.
func TestConcurrentAppendsAreWhole(t *testing.T) {
	dir := t.TempDir()
	j, err := Create(dir, Header{RunID: "r1", Command: "burst"})
	must(t, err)
	defer func() { _ = j.Close() }()
	var wg sync.WaitGroup
	for i := range 8 {
		wg.Go(func() {
			for a := range 20 {
				c := call(fmt.Sprintf("k%d-%d", i, a), "d")
				c.Hosts = []string{strings.Repeat("h", 512)}
				ok(t, func() error { _, e := j.Intent(c); return e }())
			}
		})
	}
	wg.Wait()
	eq(t, len(lines(t, j)), 1+160) // header plus every whole intent
}

// TestStallIsolatedAndFreezeNeverHangs: a stuck append blocks only its own
// caller, and Freeze returns at once so a drain never hangs on it.
func TestStallIsolatedAndFreezeNeverHangs(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		dir := t.TempDir()
		j, err := Create(dir, Header{RunID: "r1", Command: "burst"})
		must(t, err)
		defer func() { _ = j.Close() }()

		stall := make(chan struct{})
		j.Inject(func(*os.File, []byte) error { <-stall; return errors.New("injected: volume gone") })
		stuck := make(chan error)
		go func() { _, e := j.Intent(call("k1", "d1")); stuck <- e }()
		time.Sleep(time.Second) // another goroutine runs while the append stalls
		queued := make(chan error)
		go func() { _, e := j.Intent(call("k2", "d2")); queued <- e }()
		j.Freeze() // returns while the first append is stuck
		close(stall)
		is(t, <-stuck, ErrIO)
		is(t, <-queued, ErrFrozen)
		eq(t, tags(t, j), "header")
	})
}
