/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"errors"
	"io"
	"net"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewRedisFromConfig(t *testing.T) {
	if backend != "" && backend != "Redis" {
		t.Skip("Redis backend not selected")
	}
	_, baseline, err := startRedis()
	require.NoError(t, err)
	t.Cleanup(func() { assert.NoError(t, baseline.rdb.Close()) })

	for _, host := range []string{"::1", "[::1]"} {
		t.Run(host, func(t *testing.T) {
			port := forwardBackendOverIPv6(t, baseline.rdb.Options().Addr)
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()

			storage, err := newRedisFromConfig(ctx, RedisConfig{IP: host, Port: port})
			require.NoError(t, err)
			t.Cleanup(func() { assert.NoError(t, storage.rdb.Close()) })
			namespaces, err := storage.ListNamespaces(ctx)
			require.NoError(t, err)
			require.Contains(t, namespaces, defaultNamespace)
		})
	}
}

func TestNewEtcd(t *testing.T) {
	if backend != "" && backend != "Etcd" {
		t.Skip("Etcd backend not selected")
	}
	_, baseline, err := startEtcd()
	require.NoError(t, err)
	t.Cleanup(func() { assert.NoError(t, baseline.etcdDB.Close()) })

	for _, host := range []string{"::1", "[::1]"} {
		t.Run(host, func(t *testing.T) {
			port := forwardBackendOverIPv6(t, baseline.etcdDB.Endpoints()[0])
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()

			storage, err := newEtcd(ctx, host, port, nil, nil, false)
			require.NoError(t, err)
			t.Cleanup(func() { assert.NoError(t, storage.etcdDB.Close()) })
			namespaces, err := storage.ListNamespaces(ctx)
			require.NoError(t, err)
			require.Contains(t, namespaces, defaultNamespace)
		})
	}
}

// Listen on IPv6 locally so these tests do not depend on Docker publishing
// the backend's port on IPv6.
func forwardBackendOverIPv6(t *testing.T, target string) string {
	t.Helper()
	listener, err := net.ListenTCP("tcp6", &net.TCPAddr{IP: net.IPv6loopback})
	if errors.Is(err, syscall.EAFNOSUPPORT) || errors.Is(err, syscall.EADDRNOTAVAIL) {
		t.Skipf("IPv6 loopback is unavailable: %v", err)
	}
	require.NoError(t, err)
	closeConnection := func(connection net.Conn) {
		err := connection.Close()
		if !errors.Is(err, net.ErrClosed) {
			assert.NoError(t, err)
		}
	}
	ctx, cancel := context.WithCancel(t.Context())
	var connections []net.Conn
	var workers sync.WaitGroup
	done := make(chan struct{})
	t.Cleanup(func() {
		cancel()
		assert.NoError(t, listener.Close())
		<-done
		for _, connection := range connections {
			closeConnection(connection)
		}
		workers.Wait()
	})
	go func() {
		defer close(done)
		// gRPC can replace a connection while switching balancers during startup.
		for {
			client, err := listener.Accept()
			if err != nil {
				if !errors.Is(err, net.ErrClosed) {
					assert.NoError(t, err)
				}
				return
			}
			connections = append(connections, client)
			upstream, err := (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "tcp", target)
			if err != nil {
				closeConnection(client)
				if ctx.Err() == nil {
					assert.NoError(t, err)
				}
				continue
			}
			connections = append(connections, upstream)
			workers.Go(func() {
				clientCopy := make(chan struct{})
				var clientError error
				go func() {
					defer close(clientCopy)
					_, clientError = io.Copy(upstream, client)
					closeConnection(upstream)
				}()
				_, serverError := io.Copy(client, upstream)
				closeConnection(client)
				<-clientCopy
				for _, err := range []error{clientError, serverError} {
					// Replacing a gRPC connection can reset it before forwarding finishes.
					if err != nil && !errors.Is(err, net.ErrClosed) &&
						!errors.Is(err, syscall.ECONNRESET) && !errors.Is(err, syscall.EPIPE) {
						assert.NoError(t, err)
					}
				}
			})
		}
	}()
	_, port, err := net.SplitHostPort(listener.Addr().String())
	require.NoError(t, err)
	return port
}
