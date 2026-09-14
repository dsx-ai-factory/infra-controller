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
		// Every inventory publishes through the shared collector, which buffers items across
		// Core pages, so a page size that does not divide the Core fetch page is accepted.
		{"30 accepted (does not divide 100)", 30, false},
		{"99 accepted (does not divide 100)", 99, false},
		{"20 accepted", 20, false},
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
