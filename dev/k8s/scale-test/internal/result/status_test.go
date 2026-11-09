// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

import (
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// TestStatusOfIsSmallAndCarriesTheVerdict proves the ConfigMap status carries the verdict, exit code, ids, reasons and
// the PVC reference, and nothing else: the stage, invariant, journal and clock detail stay in the authoritative file.
func TestStatusOfIsSmallAndCarriesTheVerdict(t *testing.T) {
	s := StatusOf(sample())
	if s.Verdict != Failed || s.ExitCode != 1 || s.RunID != "r1" || s.ExecutionID != "r1-abcde" {
		t.Errorf("status omits the verdict or ids: %+v", s)
	}
	if s.Result.Path != "/journal/r1.result.json" {
		t.Errorf("status does not reference the result file: %q", s.Result.Path)
	}
	raw, err := jsonx.Encode(s)
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]json.RawMessage
	_ = json.Unmarshal(raw, &got)
	for _, big := range []string{"stages", "invariants", "journal", "clock", "lock"} {
		if _, has := got[big]; has {
			t.Errorf("the small status must not carry %q", big)
		}
	}
	if string(got["verdict"]) != `"failed"` || string(got["reasons"]) == "null" {
		t.Errorf("the status must flatten the outcome and keep reasons a list: %s", raw)
	}
}

// TestStatusReasonsAreNeverNull proves a run with no reasons still writes a list, not null.
func TestStatusReasonsAreNeverNull(t *testing.T) {
	full := sample()
	full.Reasons = nil
	if s := StatusOf(full); s.Reasons == nil {
		t.Error("reasons must be an empty list, never nil")
	}
}
