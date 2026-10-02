// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package httpserver

import (
	"context"
	"io"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// freeLoopbackAddr picks a free loopback port and returns its address for
// the server to bind.
func freeLoopbackAddr(t *testing.T) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	addr := ln.Addr().String()
	require.NoError(t, ln.Close())
	return addr
}

func TestRun_ServesAndShutsDown(t *testing.T) {
	addr := freeLoopbackAddr(t)
	handler := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("served\n"))
	})

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- Run(ctx, addr, handler, zerolog.Nop(), "test endpoint") }()

	var resp *http.Response
	var err error
	require.Eventually(t, func() bool {
		resp, err = http.Get("http://" + addr + "/")
		return err == nil
	}, 5*time.Second, 10*time.Millisecond)
	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	require.NoError(t, resp.Body.Close())
	assert.Equal(t, http.StatusOK, resp.StatusCode)
	assert.Equal(t, "served\n", string(body))

	cancel()
	select {
	case err := <-done:
		assert.NoError(t, err, "a cancelled context is a clean shutdown")
	case <-time.After(5 * time.Second):
		t.Fatal("server did not shut down")
	}

	_, err = http.Get("http://" + addr + "/")
	assert.Error(t, err, "listener must be closed after shutdown")
}

func TestRun_ClosesConnectionWhoseBodyNeverArrives(t *testing.T) {
	addr := freeLoopbackAddr(t)
	short := defaultTimeouts
	short.read = 100 * time.Millisecond

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- run(ctx, addr, http.NotFoundHandler(), zerolog.Nop(), "test endpoint", short) }()

	var conn net.Conn
	require.Eventually(t, func() bool {
		c, err := net.Dial("tcp", addr)
		if err != nil {
			return false
		}
		conn = c
		return true
	}, 5*time.Second, 10*time.Millisecond)
	defer conn.Close()

	// Declare a body and never send it. The handler does not read bodies, so
	// the server drains the declared body before it responds and must give
	// up at the read timeout instead of waiting for the client.
	_, err := io.WriteString(conn, "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 10\r\n\r\n")
	require.NoError(t, err)
	require.NoError(t, conn.SetReadDeadline(time.Now().Add(5*time.Second)))
	raw, err := io.ReadAll(conn)
	require.NoError(t, err, "the server must close the connection; a client-side deadline means it kept waiting for the body")
	assert.Contains(t, string(raw), "HTTP/1.1 404")

	cancel()
	select {
	case err := <-done:
		assert.NoError(t, err)
	case <-time.After(5 * time.Second):
		t.Fatal("server did not shut down")
	}
}

func TestRun_ListenError(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	defer ln.Close()

	err = Run(context.Background(), ln.Addr().String(), http.NotFoundHandler(), zerolog.Nop(), "test endpoint")
	require.Error(t, err, "binding an occupied port must fail")
}
