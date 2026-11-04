// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package jsonx

import (
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
