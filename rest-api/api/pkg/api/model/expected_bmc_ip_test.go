// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestValidateExpectedBmcIPAddress(t *testing.T) {
	tests := []struct {
		name    string
		address string
		valid   bool
	}{
		{name: "private IPv4", address: "192.168.1.10", valid: true},
		{name: "unique-local IPv6", address: "fd00::10", valid: true},
		{name: "mapped private IPv4", address: "::ffff:192.168.1.10", valid: true},
		{name: "loopback IPv4 retains existing behavior", address: "127.0.0.1", valid: true},
		{name: "link-local IPv4 retains existing behavior", address: "169.254.1.1", valid: true},
		{name: "loopback IPv6 retains existing behavior", address: "::1", valid: true},
		{name: "link-local IPv6 retains existing behavior", address: "fe80::1", valid: true},
		{name: "unspecified IPv4", address: "0.0.0.0"},
		{name: "mapped unspecified IPv4", address: "::ffff:0.0.0.0"},
		{name: "limited broadcast", address: "255.255.255.255"},
		{name: "mapped limited broadcast", address: "::ffff:255.255.255.255"},
		{name: "multicast IPv4", address: "224.0.0.1"},
		{name: "unspecified IPv6", address: "::"},
		{name: "multicast IPv6", address: "ff02::1"},
		{name: "malformed", address: "not-an-ip"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.valid, validateExpectedBmcIPAddress(tc.address) == nil)
		})
	}
}
