// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"bytes"
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
		{[]string{"fleet-up"}, exitNotRun, "", "scale-test: unknown command \"fleet-up\"\n" + usage + "\n"},
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
