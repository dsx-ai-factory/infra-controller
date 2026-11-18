// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package jsonx

import (
	"encoding/json"
	"math"
	"strings"
	"testing"
)

type (
	color string
	box   struct {
		X any `json:"x"`
	}
)

func init() { Enum[color]("red") }

// Values behind an interface (result settings, detail, observed) get the
// same checks, and the error never holds the refused text.
func TestEncodeChecksInsideInterfaces(t *testing.T) {
	const secret = "s3cr3t"
	for name, v := range map[string]any{
		"not UTF-8 in a map":     box{map[string]any{"k": []any{secret + "\xff"}}},
		"not UTF-8 key":          box{map[string]any{secret + "\xff": 1}},
		"unknown enum in a map":  box{map[string]any{"k": color(secret)}},
		"negative zero in a map": box{map[string]any{"k": []any{math.Copysign(0, -1)}}},
		"negative zero alone":    math.Copysign(0, -1),
	} {
		if out, err := Encode(v); err == nil || strings.Contains(err.Error(), secret) {
			t.Errorf("%s: wrote %s, %v", name, out, err)
		}
	}
	if _, err := Encode(box{map[string]any{"k": []any{1.5, math.Copysign(0, -1)}}}); err == nil || !strings.Contains(err.Error(), "x{0}[1]") {
		t.Errorf("negative zero error %v does not name its path", err)
	}
	if out, err := Encode(box{map[string]any{"k": []any{color("red"), "é"}}}); err != nil {
		t.Errorf("valid nested values: %s, %v", out, err)
	}
}

// A json:"-" field is never on the wire, two in one struct included (as in result.Outcome): Encode leaves both out and
// Decode skips a key named for either, leaving them zero.
func TestSkippedFieldsStayOffTheWire(t *testing.T) {
	type outcome struct {
		Verdict string   `json:"verdict"`
		Reasons []string `json:"-"`
		Detail  []byte   `json:"-"`
	}
	if out, err := Encode(outcome{"go", []string{"r"}, []byte("{}")}); err != nil || string(out) != `{"verdict":"go"}` {
		t.Errorf("Encode = %s, %v", out, err)
	}
	var got outcome
	if err := Decode([]byte(`{"verdict":"go","-":["r"],"Reasons":["r"],"Detail":"e30="}`), &got); err != nil ||
		got.Verdict != "go" || got.Reasons != nil || got.Detail != nil {
		t.Errorf("Decode = %+v, %v", got, err)
	}
}

// A json.RawMessage, such as a test's findings, reads back as its bytes; null and malformed JSON are refused without
// repeating the input.
func TestRawMessageKeepsItsBytes(t *testing.T) {
	var got struct {
		Detail json.RawMessage `json:"detail,omitempty"`
	}
	if err := Decode([]byte(`{"detail":{"ready":true,"checks":[{"name":"probe"}]}}`), &got); err != nil ||
		string(got.Detail) != `{"ready":true,"checks":[{"name":"probe"}]}` {
		t.Errorf("Decode = %s, %v", got.Detail, err)
	}
	for _, bad := range []string{`{"detail":null}`, `{"detail":{"secret":}}`} {
		if err := Decode([]byte(bad), &got); err == nil || strings.Contains(err.Error(), "secret") {
			t.Errorf("%s: err = %v, want a refusal without the input", bad, err)
		}
	}
}
