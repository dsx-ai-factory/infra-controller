// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"bytes"
	"context"
	"slices"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/authz"
	flowmetrics "github.com/NVIDIA/infra-controller/rest-api/flow/internal/metrics"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

func TestInterceptorChainsPropagateAuthorizationToRecovery(t *testing.T) {
	authorizer, err := authz.New(authz.Config{Mode: authz.ModeAudit})
	require.NoError(t, err)

	t.Run("unary", func(t *testing.T) {
		var output bytes.Buffer
		restoreLogger(t, &output)
		registry := prometheus.NewRegistry()
		serverMetrics := flowmetrics.NewRPCServerMetrics(registry)

		handler := chainUnaryInterceptors(
			unaryServerInterceptors(authorizer, serverMetrics),
			&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Test"},
			func(context.Context, any) (any, error) {
				panic("boom")
			},
		)
		_, err := handler(context.Background(), struct{}{})

		require.Equal(t, codes.Internal, status.Code(err))
		assert.Contains(t, output.String(), `"service_identity":"`+authz.AuditUnidentifiedIdentity+`"`)
		require.NoError(t, testutil.GatherAndCompare(
			registry,
			strings.NewReader(`# HELP nico_flow_grpc_server_requests_total Total number of completed Flow gRPC server requests.
# TYPE nico_flow_grpc_server_requests_total counter
nico_flow_grpc_server_requests_total{grpc_code="Internal",grpc_method="Test",grpc_service="flow.v1.Flow",grpc_type="unary"} 1
`),
			"nico_flow_grpc_server_requests_total",
		))
	})

	t.Run("stream", func(t *testing.T) {
		var output bytes.Buffer
		restoreLogger(t, &output)
		registry := prometheus.NewRegistry()
		serverMetrics := flowmetrics.NewRPCServerMetrics(registry)

		handler := chainStreamInterceptors(
			streamServerInterceptors(authorizer, serverMetrics),
			&grpc.StreamServerInfo{FullMethod: "/flow.v1.Flow/TestStream"},
			func(any, grpc.ServerStream) error {
				panic("boom")
			},
		)
		err := handler(struct{}{}, &chainTestServerStream{ctx: context.Background()})

		require.Equal(t, codes.Internal, status.Code(err))
		assert.Contains(t, output.String(), `"service_identity":"`+authz.AuditUnidentifiedIdentity+`"`)
		require.NoError(t, testutil.GatherAndCompare(
			registry,
			strings.NewReader(`# HELP nico_flow_grpc_server_requests_total Total number of completed Flow gRPC server requests.
# TYPE nico_flow_grpc_server_requests_total counter
nico_flow_grpc_server_requests_total{grpc_code="Internal",grpc_method="TestStream",grpc_service="flow.v1.Flow",grpc_type="stream"} 1
`),
			"nico_flow_grpc_server_requests_total",
		))
	})
}

func chainUnaryInterceptors(
	interceptors []grpc.UnaryServerInterceptor,
	info *grpc.UnaryServerInfo,
	handler grpc.UnaryHandler,
) grpc.UnaryHandler {
	for _, interceptor := range slices.Backward(interceptors) {
		next := handler
		handler = func(ctx context.Context, req any) (any, error) {
			return interceptor(ctx, req, info, next)
		}
	}
	return handler
}

func chainStreamInterceptors(
	interceptors []grpc.StreamServerInterceptor,
	info *grpc.StreamServerInfo,
	handler grpc.StreamHandler,
) grpc.StreamHandler {
	for _, interceptor := range slices.Backward(interceptors) {
		next := handler
		handler = func(srv any, stream grpc.ServerStream) error {
			return interceptor(srv, stream, info, next)
		}
	}
	return handler
}

func restoreLogger(t *testing.T, output *bytes.Buffer) {
	t.Helper()
	original := log.Logger
	log.Logger = zerolog.New(output)
	t.Cleanup(func() {
		log.Logger = original
	})
}

type chainTestServerStream struct {
	ctx context.Context
}

func (*chainTestServerStream) SetHeader(metadata.MD) error  { return nil }
func (*chainTestServerStream) SendHeader(metadata.MD) error { return nil }
func (*chainTestServerStream) SetTrailer(metadata.MD)       {}
func (s *chainTestServerStream) Context() context.Context   { return s.ctx }
func (*chainTestServerStream) SendMsg(any) error            { return nil }
func (*chainTestServerStream) RecvMsg(any) error            { return nil }
