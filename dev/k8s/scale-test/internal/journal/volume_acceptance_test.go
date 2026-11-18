// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build acceptance

// These are the real-cluster entry points the runner chart runs as two helm
// test Jobs on one claim: a writer and a verifier bound to the same run ID. The
// write and verify logic, and its offline regressions, live in the untagged
// file so the ordinary race gate exercises them. Both targets fail, never skip,
// when their environment is unset, so a missing mount cannot read as a pass.
package journal_test

import (
	"os"
	"testing"
)

// journalEnv returns a required chart-set variable, failing rather than
// skipping when it is unset.
func journalEnv(t *testing.T, key string) string {
	t.Helper()
	v := os.Getenv(key)
	if v == "" {
		t.Fatalf("%s must be set to run the journal volume proof", key)
	}
	return v
}

// TestJournalWritesToVolume writes the run's journal to the mounted claim. It is
// the lower-weight Job, so it precedes the verifier.
func TestJournalWritesToVolume(t *testing.T) {
	dir := journalEnv(t, "SCALE_TEST_JOURNAL_DIR")
	id := journalEnv(t, "SCALE_TEST_JOURNAL_RUN_ID")
	if err := writeRunJournal(dir, id); err != nil {
		t.Fatalf("write journal for run %s to %s: %v", id, dir, err)
	}
	t.Logf("wrote journal for run %s to %s", id, dir)
}

// TestJournalReadsBackFromVolume mounts the same claim read-only and proves the
// writer's journal survived whole and belongs to this run.
func TestJournalReadsBackFromVolume(t *testing.T) {
	dir := journalEnv(t, "SCALE_TEST_JOURNAL_DIR")
	id := journalEnv(t, "SCALE_TEST_JOURNAL_RUN_ID")
	if err := verifyRunJournal(dir, id); err != nil {
		t.Fatalf("verify journal for run %s from %s: %v", id, dir, err)
	}
	t.Logf("verified journal for run %s read back whole from %s", id, dir)
}
