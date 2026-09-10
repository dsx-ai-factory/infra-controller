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

func TestRun_ServesAndShutsDown(t *testing.T) {
	// Pick a free loopback port, then hand the address to the server.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	addr := ln.Addr().String()
	require.NoError(t, ln.Close())

	handler := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("served\n"))
	})

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- Run(ctx, addr, handler, zerolog.Nop(), "test endpoint") }()

	var resp *http.Response
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

func TestRun_ListenError(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	defer ln.Close()

	err = Run(context.Background(), ln.Addr().String(), http.NotFoundHandler(), zerolog.Nop(), "test endpoint")
	require.Error(t, err, "binding an occupied port must fail")
}
