// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshclient

import (
	"bufio"
	"errors"
	"net"
	"syscall"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewWithPort(t *testing.T) {
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
			ip := net.ParseIP(tt.ip)
			listener, err := net.ListenTCP(tt.network, &net.TCPAddr{IP: ip})
			if tt.network == "tcp6" && (errors.Is(err, syscall.EAFNOSUPPORT) || errors.Is(err, syscall.EADDRNOTAVAIL)) {
				t.Skipf("IPv6 loopback is unavailable: %v", err)
			}
			require.NoError(t, err)
			t.Cleanup(func() {
				assert.NoError(t, listener.Close())
			})

			deadline := time.Now().Add(5 * time.Second)
			err = listener.SetDeadline(deadline)
			require.NoError(t, err)

			serverDone := make(chan struct{})
			var serverErr error
			go func() {
				defer close(serverDone)
				conn, err := listener.AcceptTCP()
				if err != nil {
					serverErr = err
					return
				}
				defer func() {
					assert.NoError(t, conn.Close())
				}()

				serverErr = conn.SetDeadline(deadline)
				if serverErr != nil {
					return
				}
				// Read the client's SSH version before closing so the handshake
				// fails only after the constructor reaches this listener.
				_, serverErr = bufio.NewReader(conn).ReadString('\n')
			}()
			cred := credential.New("admin", "password")
			n := &nvos.NVOS{IP: ip, Credential: &cred}
			port := listener.Addr().(*net.TCPAddr).Port
			client, err := NewWithPort(t.Context(), n, port)
			<-serverDone

			require.NoError(t, serverErr, "SSH client must reach the configured listener")
			assert.Nil(t, client)
			assert.ErrorContains(t, err, "ssh: handshake failed: EOF")
		})
	}
}
