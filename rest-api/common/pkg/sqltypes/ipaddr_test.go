// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sqltypes

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

// TestIPAddrScan covers (*IPAddr).Scan, with emphasis on the CIDR-notation
// stripping that PostgreSQL's inet type can return (e.g. "192.168.1.100/24").
// The strip is implemented with strings.LastIndexByte, so these cases pin that
// behavior, including the IPv6 path (a single '/' prefix delimiter, never a
// colon) and the bare "/" edge case, which strips to "" and is then rejected
// by net.ParseIP.
func TestIPAddrScan(t *testing.T) {
	tests := []struct {
		name    string
		src     interface{}
		wantIP  string // expected IPAddr.String(); ignored when wantErr is true
		wantErr bool
	}{
		{name: "plain IPv4 string", src: "192.168.1.100", wantIP: "192.168.1.100"},
		{name: "IPv4 with CIDR prefix", src: "192.168.1.100/24", wantIP: "192.168.1.100"},
		{name: "IPv4 with /32", src: "10.0.0.2/32", wantIP: "10.0.0.2"},
		{name: "IPv6 plain", src: "2001:db8::1", wantIP: "2001:db8::1"},
		{name: "IPv6 with CIDR prefix", src: "2001:db8::1/64", wantIP: "2001:db8::1"},
		{name: "[]byte with CIDR prefix", src: []byte("172.16.0.5/16"), wantIP: "172.16.0.5"},
		{name: "[]byte plain", src: []byte("8.8.8.8"), wantIP: "8.8.8.8"},
		{name: "nil src clears to <nil>", src: nil, wantIP: "<nil>"},
		{name: "unsupported type errors", src: 42, wantErr: true},
		{name: "unparseable string errors", src: "not-an-ip", wantErr: true},
		{name: "bare slash strips to empty and errors", src: "/", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var ip IPAddr
			err := ip.Scan(tt.src)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			if !assert.NoError(t, err) {
				return
			}
			assert.Equal(t, tt.wantIP, ip.String())
		})
	}
}
