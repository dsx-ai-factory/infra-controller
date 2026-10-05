// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestRPCServerMetrics_UnaryServerInterceptor(t *testing.T) {
	registry := prometheus.NewRegistry()
	metrics := NewRPCServerMetrics(registry)
	interceptor := metrics.UnaryServerInterceptor()

	_, err := interceptor(
		context.Background(),
		struct{}{},
		&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/CreateTask"},
		func(context.Context, any) (any, error) {
			return nil, status.Error(codes.InvalidArgument, "invalid task")
		},
	)
	require.Error(t, err)

	assert.Equal(t, float64(1), testutil.ToFloat64(
		metrics.requests.WithLabelValues("flow.v1.Flow", "CreateTask", "unary", "InvalidArgument"),
	))
	assert.Equal(t, 1, testutil.CollectAndCount(metrics.duration))
}

func TestRPCServerMetrics_StreamServerInterceptor(t *testing.T) {
	registry := prometheus.NewRegistry()
	metrics := NewRPCServerMetrics(registry)
	interceptor := metrics.StreamServerInterceptor()

	err := interceptor(
		struct{}{},
		nil,
		&grpc.StreamServerInfo{FullMethod: "/flow.v1.Flow/WatchTasks"},
		func(any, grpc.ServerStream) error { return nil },
	)
	require.NoError(t, err)

	assert.Equal(t, float64(1), testutil.ToFloat64(
		metrics.requests.WithLabelValues("flow.v1.Flow", "WatchTasks", "stream", "OK"),
	))
}

func TestServer_ServesAndShutsDown(t *testing.T) {
	registry := prometheus.NewRegistry()
	metric := prometheus.NewGauge(prometheus.GaugeOpts{Name: "flow_test_metric"})
	registry.MustRegister(metric)
	metric.Set(1)

	server, err := NewServer("127.0.0.1:0", registry)
	require.NoError(t, err)

	serveErr := make(chan error, 1)
	go func() { serveErr <- server.Serve() }()

	request, err := http.NewRequestWithContext(t.Context(), http.MethodGet, "http://"+server.Addr().String()+"/metrics", nil)
	require.NoError(t, err)
	client := &http.Client{Timeout: 5 * time.Second}
	response, err := client.Do(request)
	require.NoError(t, err)
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	require.NoError(t, err)
	assert.Equal(t, http.StatusOK, response.StatusCode)
	assert.Contains(t, string(body), "flow_test_metric 1")

	shutdownCtx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	require.NoError(t, server.Shutdown(shutdownCtx))
	require.NoError(t, <-serveErr)
}

func TestNewRegistry_RegistersRuntimeAndRPCMetrics(t *testing.T) {
	registry, _ := NewRegistry()
	families, err := registry.Gather()
	require.NoError(t, err)

	names := make(map[string]struct{}, len(families))
	for _, family := range families {
		names[family.GetName()] = struct{}{}
	}
	assert.Contains(t, names, "go_goroutines")
	assert.Contains(t, names, "process_cpu_seconds_total")
}

func TestServer_ReadTimeout(t *testing.T) {
	server, err := NewServer("127.0.0.1:0", prometheus.NewRegistry())
	require.NoError(t, err)
	require.Positive(t, server.server.ReadTimeout)
	require.Positive(t, server.server.WriteTimeout)
	require.Positive(t, server.server.IdleTimeout)
	server.server.ReadTimeout = 100 * time.Millisecond
	done := make(chan error, 1)
	go func() { done <- server.Serve() }()
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		require.NoError(t, server.Shutdown(ctx))
		require.NoError(t, <-done)
	})
	conn, err := net.DialTimeout("tcp", server.Addr().String(), time.Second)
	require.NoError(t, err)
	defer conn.Close()
	require.NoError(t, conn.SetDeadline(time.Now().Add(2*time.Second)))
	// Keep the connection persistent so the server attempts to drain the missing body.
	_, err = fmt.Fprint(conn, "GET /metrics HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\n\r\n")
	require.NoError(t, err)
	_, err = io.ReadAll(conn)
	require.NoError(t, err, "server must close the stalled request before the client deadline")
}
