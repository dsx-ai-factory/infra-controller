// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"errors"
	"net"
	"syscall"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestIsReachable(t *testing.T) {
	tests := []struct {
		name    string
		network string
		ip      string
	}{
		{name: "IPv4", network: "tcp4", ip: "127.0.0.1"},
		{name: "IPv6", network: "tcp6", ip: "::1"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			listener, err := net.ListenTCP(tt.network, &net.TCPAddr{IP: net.ParseIP(tt.ip)})
			if tt.network == "tcp6" && (errors.Is(err, syscall.EAFNOSUPPORT) || errors.Is(err, syscall.EADDRNOTAVAIL)) {
				t.Skipf("IPv6 loopback is unavailable: %v", err)
			}
			require.NoError(t, err)
			t.Cleanup(func() {
				_ = listener.Close()
			})

			port := listener.Addr().(*net.TCPAddr).Port
			assert.True(t, IsReachable(tt.ip, port), "the configured listener is reachable")
			require.NoError(t, listener.Close())
			assert.False(t, IsReachable(tt.ip, port), "the closed listener is unreachable")
		})
	}
}
