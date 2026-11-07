// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package redfish

import (
	"net"
	"testing"
)

func TestBuildEndpoint(t *testing.T) {
	cases := []struct {
		name string
		ip   string
		port int
		want string
	}{
		{"ipv4 default port", "192.0.2.10", 443, "https://192.0.2.10:443"},
		{"ipv4 custom port", "192.0.2.10", 8443, "https://192.0.2.10:8443"},
		{"ipv6 default port", "2001:db8::1", 443, "https://[2001:db8::1]:443"},
		{"ipv6 custom port", "2001:db8::1", 8443, "https://[2001:db8::1]:8443"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ip := net.ParseIP(tc.ip)
			if ip == nil {
				t.Fatalf("invalid test IP %q", tc.ip)
			}
			got := buildEndpoint(ip, tc.port)
			if got != tc.want {
				t.Errorf("buildEndpoint(%q, %d) = %q, want %q", tc.ip, tc.port, got, tc.want)
			}
		})
	}
}
