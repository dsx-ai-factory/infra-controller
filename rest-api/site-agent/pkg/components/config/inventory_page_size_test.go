// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import "testing"

func TestValidateInventoryCloudPageSize(t *testing.T) {
	cases := []struct {
		name    string
		size    int
		wantErr bool
	}{
		{"zero rejected", 0, true},
		{"negative rejected", -1, true},
		{"one is the minimum valid value", 1, false},
		{"historical default 25", 25, false},
		{"deployed value 50", 50, false},
		{"100 is the maximum valid value", 100, false},
		{"101 rejected (just over max)", 101, true},
		{"far over max rejected", 100000, true},
		// Non-divisors of 100 break machine-inventory pagination totals across Core page
		// boundaries (see the function doc comment) -- rejected even though they're within
		// [1, 100].
		{"40 rejected (does not divide 100)", 40, true},
		{"30 rejected (does not divide 100)", 30, true},
		{"33 rejected (does not divide 100)", 33, true},
		{"99 rejected (does not divide 100)", 99, true},
		{"20 accepted (divides 100)", 20, false},
		{"10 accepted (divides 100)", 10, false},
		{"4 accepted (divides 100)", 4, false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			err := validateInventoryCloudPageSize(c.size)
			if c.wantErr && err == nil {
				t.Errorf("size=%d: expected error, got nil", c.size)
			}
			if !c.wantErr && err != nil {
				t.Errorf("size=%d: expected no error, got %v", c.size, err)
			}
		})
	}
}
