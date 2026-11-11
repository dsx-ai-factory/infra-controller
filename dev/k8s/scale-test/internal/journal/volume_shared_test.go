// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// This file carries the journal volume proof's write and verify logic and its
// offline regressions. It has no build tag, so the regressions run in the
// ordinary race gate; the acceptance-tagged entry points call the same two
// functions against a real claim. Keeping the checks here means a verifier
// that drops the checksum, record or run-ID guard fails an offline test, not
// only an un-run cluster Job.
package journal_test

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
)

const (
	proofName      = "proof.json"
	journalExt     = ".journal.jsonl"
	journalCommand = "journal-volume-proof"
	opKind         = "expectedMachines"
	journalOps     = 3                // intent+accepted-receipt pairs the writer makes
	wantRecords    = 1 + 2*journalOps // header plus each pair: a fixed seven
	maxRunIDLen    = 128              // the run ID is a file-name base
	maxJournalRead = 1 << 16          // the fixture is ~1 KiB; cap the read anyway
	maxProofRead   = 4 << 10          // proof.json is a few fields
)

// proof is the writer's record of the journal it wrote. The verifier checks it
// against the run ID the chart binds, not against the volume alone, so a stale
// but internally consistent proof cannot pass as the current run.
type proof struct {
	RunID  string `json:"runId"`
	SHA256 string `json:"sha256"`
	Bytes  int    `json:"bytes"`
}

// rec is the subset of a journal line the structural check reads. Timestamps,
// sequence numbers and digests are deliberately ignored: the fixed shape is
// what proves the file is whole, not those volatile fields.
type rec struct {
	Entry   string `json:"entry"`
	RunID   string `json:"runId"`
	Key     string `json:"key"`
	Kind    string `json:"kind"`
	Op      uint64 `json:"op"`
	Outcome struct {
		Kind string `json:"kind"`
	} `json:"outcome"`
}

func opKey(runID string, i int) string { return fmt.Sprintf("%s-op-%d", runID, i) }

// safeRunID rejects an empty or path-bearing run ID before it is joined into a
// file path or used as a journal name. A separator is rejected explicitly:
// filepath.Base("/") is "/", so the base check alone would let the root
// separator through.
func safeRunID(id string) error {
	if id == "" || id == "." || id == ".." || strings.ContainsRune(id, '/') || len(id) > maxRunIDLen {
		return fmt.Errorf("unsafe run ID %q", id)
	}
	return nil
}

// readCapped reads at most limit bytes and fails if the file is larger, so a
// corrupt or runaway file cannot be slurped whole.
func readCapped(path string, limit int) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()
	data, err := io.ReadAll(io.LimitReader(f, int64(limit)+1))
	if err != nil {
		return nil, err
	}
	if len(data) > limit {
		return nil, fmt.Errorf("file %s too large (over %d bytes)", filepath.Base(path), limit)
	}
	return data, nil
}

// records splits a newline-terminated journal into its lines, requiring a final
// newline and no empty interior line, so a torn write cannot read as records.
func records(data []byte) ([][]byte, error) {
	parts := bytes.Split(data, []byte{'\n'})
	if n := len(parts); n == 0 || len(parts[n-1]) != 0 {
		return nil, fmt.Errorf("journal does not end with a newline")
	}
	return parts[:len(parts)-1], nil
}

// validateRecords proves the journal is the exact fixture the writer makes:
// seven whole-JSON records, a header for wantID, then three intent/accepted
// pairs with matching keys and operation numbers. It is the check a bare
// checksum cannot do, since a shorter file with recomputed metadata still
// matches its own checksum.
func validateRecords(data []byte, wantID string) error {
	lines, err := records(data)
	if err != nil {
		return err
	}
	if len(lines) != wantRecords {
		return fmt.Errorf("journal has %d records, want %d", len(lines), wantRecords)
	}
	recs := make([]rec, wantRecords)
	for i, ln := range lines {
		// Unmarshal already requires one complete, valid JSON object per line,
		// so a torn line fails here.
		if err := json.Unmarshal(ln, &recs[i]); err != nil {
			return fmt.Errorf("record %d: %w", i+1, err)
		}
	}
	if h := recs[0]; h.Entry != "header" || h.RunID != wantID {
		return fmt.Errorf("header entry=%q runId=%q, want header/%s", h.Entry, h.RunID, wantID)
	}
	for i := 0; i < journalOps; i++ {
		in, rc := recs[1+2*i], recs[2+2*i]
		if in.Entry != "intent" || in.Key != opKey(wantID, i) || in.Kind != opKind {
			return fmt.Errorf("record %d intent entry=%q key=%q kind=%q", 2+2*i, in.Entry, in.Key, in.Kind)
		}
		if rc.Entry != "receipt" || rc.Outcome.Kind != journal.Accepted || rc.Op != in.Op {
			return fmt.Errorf("record %d receipt entry=%q outcome=%q op=%d (intent op %d)", 3+2*i, rc.Entry, rc.Outcome.Kind, rc.Op, in.Op)
		}
	}
	return nil
}

// writeProof records the proof atomically: a temp file synced and renamed into
// place, then the directory synced, so the verifier never reads a half-written
// record and the record survives a crash.
func writeProof(dir string, p proof) error {
	b, err := json.Marshal(p)
	if err != nil {
		return err
	}
	b = append(b, '\n')
	tmp := filepath.Join(dir, proofName+".tmp")
	f, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, err := f.Write(b); err != nil {
		_ = f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		_ = f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmp, filepath.Join(dir, proofName)); err != nil {
		return err
	}
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer func() { _ = d.Close() }()
	return d.Sync()
}

// writeRunJournal writes the fixed journal for runID through the real journal
// package, with a fsync per record, self-checks its shape, then records the
// proof. The self-check means the writer cannot certify a truncated baseline.
func writeRunJournal(dir, runID string) error {
	if err := safeRunID(runID); err != nil {
		return err
	}
	j, err := journal.Create(dir, journal.Header{RunID: runID, Command: journalCommand})
	if err != nil {
		return err
	}
	for i := 0; i < journalOps; i++ {
		key := opKey(runID, i)
		if err := j.Claim(key); err != nil {
			_ = j.Close()
			return err
		}
		ref, err := j.Intent(journal.WriteCall{
			Scope: "test", Kind: opKind, Key: key, Idempotent: true,
			Hosts: []string{fmt.Sprintf("fm100h%d", i)}, RequestDigest: fmt.Sprintf("digest-%d", i),
		})
		if err != nil {
			_ = j.Close()
			return err
		}
		if err := j.Receipt(ref, journal.Outcome{Kind: journal.Accepted}); err != nil {
			_ = j.Close()
			return err
		}
	}
	path := j.Path()
	if err := j.Close(); err != nil {
		return err
	}
	data, err := readCapped(path, maxJournalRead)
	if err != nil {
		return err
	}
	if err := validateRecords(data, runID); err != nil {
		return fmt.Errorf("writer self-check: %w", err)
	}
	sum := sha256.Sum256(data)
	return writeProof(dir, proof{RunID: runID, SHA256: hex.EncodeToString(sum[:]), Bytes: len(data)})
}

// verifyRunJournal proves the journal for wantID survived on the volume whole:
// the proof is this run's, bounded and readable; the journal matches its byte
// length and checksum; and it is the fixed fixture for wantID. Every mismatch
// is an error, never a skip.
func verifyRunJournal(dir, wantID string) error {
	if err := safeRunID(wantID); err != nil {
		return err
	}
	pb, err := readCapped(filepath.Join(dir, proofName), maxProofRead)
	if err != nil {
		return fmt.Errorf("read proof: %w", err)
	}
	var p proof
	if err := json.Unmarshal(pb, &p); err != nil {
		return fmt.Errorf("decode proof: %w", err)
	}
	if p.RunID != wantID {
		return fmt.Errorf("proof run ID %q, want this run's %q: a stale proof", p.RunID, wantID)
	}
	// p.RunID now equals the already-validated wantID, so it is a safe base.
	data, err := readCapped(filepath.Join(dir, p.RunID+journalExt), maxJournalRead)
	if err != nil {
		return fmt.Errorf("read journal: %w", err)
	}
	if len(data) != p.Bytes {
		return fmt.Errorf("journal is %d bytes, proof recorded %d: the volume did not preserve it whole", len(data), p.Bytes)
	}
	sum := sha256.Sum256(data)
	if got := hex.EncodeToString(sum[:]); got != p.SHA256 {
		return fmt.Errorf("journal checksum %s, proof recorded %s: the bytes changed on the volume", got, p.SHA256)
	}
	return validateRecords(data, wantID)
}

// ---- offline regressions (run in the ordinary race gate) ----

func TestWriteThenVerifyRoundTrips(t *testing.T) {
	dir := t.TempDir()
	const id = "run-ok"
	if err := writeRunJournal(dir, id); err != nil {
		t.Fatalf("write: %v", err)
	}
	if err := verifyRunJournal(dir, id); err != nil {
		t.Fatalf("verify: %v", err)
	}
}

// TestVerifyCatchesTamperedVolume covers each way a persisted file can be wrong.
// Each case starts from a good write, then corrupts the volume, and the verify
// must return an error naming the guard that caught it. The checksum case
// alters a non-structural byte, so it fails only because the checksum is
// checked: drop that check and this case regresses.
func TestVerifyCatchesTamperedVolume(t *testing.T) {
	cases := []struct {
		name    string
		id      string // the run ID the verifier is bound to
		corrupt func(t *testing.T, dir, writtenID string)
		wantErr string
	}{
		{
			name: "checksum mismatch on a non-structural byte",
			id:   "run-x",
			corrupt: func(t *testing.T, dir, id string) {
				jf := filepath.Join(dir, id+journalExt)
				data := mustRead(t, jf)
				i := bytes.Index(data, []byte("digest-0"))
				if i < 0 {
					t.Fatal("fixture missing digest-0")
				}
				data[i+7] = '9' // digest-0 -> digest-9: same length, ignored by records
				mustWrite(t, jf, data)
			},
			wantErr: "checksum",
		},
		{
			name: "a removed pair with recomputed metadata",
			id:   "run-x",
			corrupt: func(t *testing.T, dir, id string) {
				jf := filepath.Join(dir, id+journalExt)
				lines := bytes.SplitAfter(mustRead(t, jf), []byte{'\n'})
				short := bytes.Join(lines[:wantRecords-2], nil) // drop the last pair
				mustWrite(t, jf, short)
				sum := sha256.Sum256(short)
				if err := writeProof(dir, proof{id, hex.EncodeToString(sum[:]), len(short)}); err != nil {
					t.Fatal(err)
				}
			},
			wantErr: "want 7",
		},
		{
			name: "a zero-line journal with a matching proof",
			id:   "run-x",
			corrupt: func(t *testing.T, dir, id string) {
				jf := filepath.Join(dir, id+journalExt)
				mustWrite(t, jf, nil)
				sum := sha256.Sum256(nil)
				if err := writeProof(dir, proof{id, hex.EncodeToString(sum[:]), 0}); err != nil {
					t.Fatal(err)
				}
			},
			wantErr: "want 7",
		},
		{
			name: "a journal with no trailing newline",
			id:   "run-x",
			corrupt: func(t *testing.T, dir, id string) {
				jf := filepath.Join(dir, id+journalExt)
				trimmed := bytes.TrimRight(mustRead(t, jf), "\n")
				mustWrite(t, jf, trimmed)
				sum := sha256.Sum256(trimmed)
				if err := writeProof(dir, proof{id, hex.EncodeToString(sum[:]), len(trimmed)}); err != nil {
					t.Fatal(err)
				}
			},
			wantErr: "does not end with a newline",
		},
		{
			name: "an oversized journal",
			id:   "run-x",
			corrupt: func(t *testing.T, dir, id string) {
				jf := filepath.Join(dir, id+journalExt)
				mustWrite(t, jf, make([]byte, maxJournalRead+1))
			},
			wantErr: "too large",
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			dir := t.TempDir()
			if err := writeRunJournal(dir, c.id); err != nil {
				t.Fatalf("seed write: %v", err)
			}
			c.corrupt(t, dir, c.id)
			err := verifyRunJournal(dir, c.id)
			if err == nil {
				t.Fatalf("verify accepted a tampered volume")
			}
			if !strings.Contains(err.Error(), c.wantErr) {
				t.Fatalf("error %q does not mention %q", err, c.wantErr)
			}
		})
	}
}

// TestVerifyFailsOnStaleRun proves a previous run's intact proof and journal do
// not pass under a new expected run ID.
func TestVerifyFailsOnStaleRun(t *testing.T) {
	dir := t.TempDir()
	if err := writeRunJournal(dir, "run-old"); err != nil {
		t.Fatalf("write: %v", err)
	}
	if err := verifyRunJournal(dir, "run-new"); err == nil || !strings.Contains(err.Error(), "stale proof") {
		t.Fatalf("want stale-proof error, got %v", err)
	}
}

// TestVerifyRejectsBadInputs covers missing files, an oversized proof and an
// unsafe run ID, none of which may panic or pass.
func TestVerifyRejectsBadInputs(t *testing.T) {
	t.Run("missing proof", func(t *testing.T) {
		if err := verifyRunJournal(t.TempDir(), "run-x"); err == nil || !strings.Contains(err.Error(), "read proof") {
			t.Fatalf("got %v", err)
		}
	})
	t.Run("missing journal", func(t *testing.T) {
		dir := t.TempDir()
		sum := sha256.Sum256(nil)
		if err := writeProof(dir, proof{"run-x", hex.EncodeToString(sum[:]), 0}); err != nil {
			t.Fatal(err)
		}
		if err := verifyRunJournal(dir, "run-x"); err == nil || !strings.Contains(err.Error(), "read journal") {
			t.Fatalf("got %v", err)
		}
	})
	t.Run("oversized proof", func(t *testing.T) {
		dir := t.TempDir()
		mustWrite(t, filepath.Join(dir, proofName), make([]byte, maxProofRead+1))
		if err := verifyRunJournal(dir, "run-x"); err == nil || !strings.Contains(err.Error(), "too large") {
			t.Fatalf("got %v", err)
		}
	})
	// safeRunID is tested directly, not through verifyRunJournal, so a missing
	// proof cannot stand in for the run-ID guard. The root separator "/" is the
	// case filepath.Base alone would wrongly accept.
	t.Run("unsafe run ID", func(t *testing.T) {
		for _, bad := range []string{"", ".", "..", "/", "a/b", "../x", strings.Repeat("a", maxRunIDLen+1)} {
			if err := safeRunID(bad); err == nil {
				t.Errorf("accepted unsafe id %q", bad)
			}
		}
		if err := safeRunID("build-123"); err != nil {
			t.Errorf("rejected a valid id: %v", err)
		}
	})
}

func mustRead(t *testing.T, path string) []byte {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func mustWrite(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
}
