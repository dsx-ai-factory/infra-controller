// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package metrics provides Prometheus instrumentation for the Flow service.
package metrics

import (
	"context"
	"errors"
	"net"
	"net/http"
	"strings"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"google.golang.org/grpc"
	"google.golang.org/grpc/status"
)

const namespace = "nico_flow"

// RPCServerMetrics records bounded-cardinality gRPC server request metrics.
type RPCServerMetrics struct {
	requests *prometheus.CounterVec
	duration *prometheus.HistogramVec
}

// NewRegistry creates the Flow Prometheus registry and its gRPC metrics.
func NewRegistry() (*prometheus.Registry, *RPCServerMetrics) {
	registry := prometheus.NewRegistry()
	registry.MustRegister(
		collectors.NewGoCollector(),
		collectors.NewProcessCollector(collectors.ProcessCollectorOpts{}),
	)

	return registry, NewRPCServerMetrics(registry)
}

// NewRPCServerMetrics registers Flow gRPC server metrics with registerer.
func NewRPCServerMetrics(registerer prometheus.Registerer) *RPCServerMetrics {
	metrics := &RPCServerMetrics{
		requests: prometheus.NewCounterVec(
			prometheus.CounterOpts{
				Namespace: namespace,
				Subsystem: "grpc_server",
				Name:      "requests_total",
				Help:      "Total number of completed Flow gRPC server requests.",
			},
			[]string{"grpc_service", "grpc_method", "grpc_type", "grpc_code"},
		),
		duration: prometheus.NewHistogramVec(
			prometheus.HistogramOpts{
				Namespace: namespace,
				Subsystem: "grpc_server",
				Name:      "handling_seconds",
				Help:      "Flow gRPC server request handling duration in seconds.",
				Buckets:   prometheus.DefBuckets,
			},
			[]string{"grpc_service", "grpc_method", "grpc_type"},
		),
	}
	registerer.MustRegister(metrics.requests, metrics.duration)

	return metrics
}

// UnaryServerInterceptor records completed unary requests and their duration.
func (m *RPCServerMetrics) UnaryServerInterceptor() grpc.UnaryServerInterceptor {
	return func(
		ctx context.Context,
		req any,
		info *grpc.UnaryServerInfo,
		handler grpc.UnaryHandler,
	) (any, error) {
		started := time.Now()
		response, err := handler(ctx, req)
		service, method := methodLabels(info.FullMethod)
		m.requests.WithLabelValues(service, method, "unary", status.Code(err).String()).Inc()
		m.duration.WithLabelValues(service, method, "unary").Observe(time.Since(started).Seconds())

		return response, err
	}
}

// StreamServerInterceptor records completed streaming requests and their duration.
func (m *RPCServerMetrics) StreamServerInterceptor() grpc.StreamServerInterceptor {
	return func(
		srv any,
		stream grpc.ServerStream,
		info *grpc.StreamServerInfo,
		handler grpc.StreamHandler,
	) error {
		started := time.Now()
		err := handler(srv, stream)
		service, method := methodLabels(info.FullMethod)
		m.requests.WithLabelValues(service, method, "stream", status.Code(err).String()).Inc()
		m.duration.WithLabelValues(service, method, "stream").Observe(time.Since(started).Seconds())

		return err
	}
}

func methodLabels(fullMethod string) (string, string) {
	trimmed := strings.TrimPrefix(fullMethod, "/")
	service, method, found := strings.Cut(trimmed, "/")
	if !found {
		return "unknown", trimmed
	}

	return service, method
}

// Server serves a Prometheus gatherer on /metrics for GET and HEAD requests.
type Server struct {
	listener net.Listener
	server   *http.Server
}

// NewServer binds address synchronously so bind errors fail startup.
func NewServer(address string, gatherer prometheus.Gatherer) (*Server, error) {
	listener, err := net.Listen("tcp", address)
	if err != nil {
		return nil, err
	}

	mux := http.NewServeMux()
	mux.Handle("GET /metrics", promhttp.HandlerFor(gatherer, promhttp.HandlerOpts{}))

	return &Server{
		listener: listener,
		server: &http.Server{
			Handler:           mux,
			ReadHeaderTimeout: 5 * time.Second,
			ReadTimeout:       10 * time.Second,
			WriteTimeout:      10 * time.Second,
			IdleTimeout:       30 * time.Second,
		},
	}, nil
}

// Addr returns the bound listener address.
func (s *Server) Addr() net.Addr {
	return s.listener.Addr()
}

// Serve blocks until the server fails or Shutdown is called.
func (s *Server) Serve() error {
	err := s.server.Serve(s.listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}

	return err
}

// Shutdown gracefully stops the metrics server.
func (s *Server) Shutdown(ctx context.Context) error {
	return s.server.Shutdown(ctx)
}
