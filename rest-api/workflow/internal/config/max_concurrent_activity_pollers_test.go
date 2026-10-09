// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import "testing"

func TestMaxConcurrentActivityPollersEdgeCases(t *testing.T) {
	cases := []struct {
		name      string
		setValue  bool // false means leave the default in place
		value     int
		wantPanic bool
		wantValue int
	}{
		{"unset uses default", false, 0, false, DefaultMaxConcurrentActivityPollers},
		{"valid 15", true, 15, false, 15},
		{"valid 20 (max)", true, 20, false, 20},
		{"valid 1 (min)", true, 1, false, 1},
		{"zero rejected", true, 0, true, 0},
		{"negative rejected", true, -5, true, 0},
		{"21 rejected (just over)", true, 21, true, 0},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			config = nil // reset package-level singleton between cases

			// NewConfig() runs outside the recover() scope: it panics on its own if required
			// DB/Temporal/secret config is missing, and conflating that startup panic with the
			// poller-specific one below would let an unrelated config problem masquerade as a
			// passing "invalid poller value" test case.
			cfg := NewConfig()

			panicked := false
			func() {
				defer func() {
					if r := recover(); r != nil {
						panicked = true
					}
				}()
				if c.setValue {
					cfg.v.Set(ConfigWorkerMaxConcurrentActivityPollers, c.value)
					cfg.Validate()
				}
			}()

			if c.wantPanic && !panicked {
				t.Errorf("value=%d: expected panic, got none (value=%d)", c.value, cfg.GetMaxConcurrentActivityPollers())
			}
			if !c.wantPanic {
				if panicked {
					t.Fatalf("value=%d: unexpected panic", c.value)
				}
				if got := cfg.GetMaxConcurrentActivityPollers(); got != c.wantValue {
					t.Errorf("value=%d: got %d, want %d", c.value, got, c.wantValue)
				}
			}
		})
	}
	config = nil // leave clean for other tests in the package
}
