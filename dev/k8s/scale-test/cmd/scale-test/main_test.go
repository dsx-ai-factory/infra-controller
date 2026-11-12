// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRunPrintsVersionAndRefusesOtherCommands(t *testing.T) {
	for _, tc := range []struct {
		args   []string
		code   int
		stdout string
		stderr string
	}{
		{[]string{"version"}, 0, "scale-test dev\n", ""},
		{nil, exitNotRun, "", usage + "\n"},
		{[]string{"fleet-ups"}, exitNotRun, "", "scale-test: unknown command \"fleet-ups\"\n" + usage + "\n"},
		{[]string{"fleet-up", "--bogus"}, exitNotRun, "", usage + "\n"},
		{[]string{"fleet-up", "extra"}, exitNotRun, "", usage + "\n"},
		{[]string{"ingestion", "extra"}, exitNotRun, "", usage + "\n"},
		{[]string{"version", "extra"}, exitNotRun, "", "scale-test: unknown command \"version extra\"\n" + usage + "\n"},
	} {
		var stdout, stderr bytes.Buffer
		code := run(tc.args, &stdout, &stderr)
		if code != tc.code || stdout.String() != tc.stdout || stderr.String() != tc.stderr {
			t.Errorf("run(%q) = %d, stdout %q, stderr %q; want %d, %q, %q",
				tc.args, code, stdout.String(), stderr.String(), tc.code, tc.stdout, tc.stderr)
		}
	}
}

// TestFleetUpRefusesABadSetupBeforeTheRun proves a missing or refused config, a missing execution ID, a dryRun on a
// command that writes nothing, and bad settings exit 2 with the cause on stderr, before any cluster call.
func TestFleetUpRefusesABadSetupBeforeTheRun(t *testing.T) {
	const good = "runId: r1\ncommand: fleet-up\nnamespace: ns\ntimeouts: {wholeRunSeconds: 3600}\n"
	for _, tc := range []struct {
		config, executionID, stderr string
	}{
		{"", "pod", "read config"},
		{"runId: r1\ncommand: version\n", "pod", "config command"},
		{good, "", "config executionId"},
		{good + "dryRun: false\n", "pod", "config dryRun: must be missing"},
		{good + "settings: {mat: {port: 1}}\n", "pod", "config settings: unknown key"},
	} {
		path := filepath.Join(t.TempDir(), "config.yaml")
		if tc.config != "" {
			if err := os.WriteFile(path, []byte(tc.config), 0o600); err != nil {
				t.Fatal(err)
			}
		}
		t.Setenv("SCALE_TEST_EXECUTION_ID", tc.executionID)
		var stdout, stderr bytes.Buffer
		code := run([]string{"fleet-up", "--config", path}, &stdout, &stderr)
		if code != exitNotRun || stdout.Len() != 0 || !strings.HasPrefix(stderr.String(), "scale-test fleet-up: "+tc.stderr) {
			t.Errorf("%q: exit %d, stdout %q, stderr %q; want 2 and %q", tc.config, code, stdout.String(), stderr.String(), tc.stderr)
		}
	}
}

// TestIngestionRefusesABadSetupBeforeTheRun: ingestion writes, so a config without dryRun, or with a bad write concurrency, exits 2 before any cluster call.
func TestIngestionRefusesABadSetupBeforeTheRun(t *testing.T) {
	const good = "runId: r1\ncommand: ingestion\nnamespace: ns\ntimeouts: {wholeRunSeconds: 3600, finalizeSeconds: 1800}\n"
	for config, want := range map[string]string{good: "config dryRun: is required", good + "dryRun: true\nsettings: {writes: {concurrency: {host: 0}}}\n": "config settings.writes"} {
		path := filepath.Join(t.TempDir(), "config.yaml")
		if err := os.WriteFile(path, []byte(config), 0o600); err != nil {
			t.Fatal(err)
		}
		t.Setenv("SCALE_TEST_EXECUTION_ID", "pod")
		var stdout, stderr bytes.Buffer
		if code := run([]string{"ingestion", "--config", path}, &stdout, &stderr); code != exitNotRun || !strings.HasPrefix(stderr.String(), "scale-test ingestion: "+want) {
			t.Errorf("exit %d, stderr %q; want 2 and %q", code, stderr.String(), want)
		}
	}
}
