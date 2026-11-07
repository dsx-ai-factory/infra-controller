// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"time"

	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// Metrics interface that defines call-back functions for RPC metrics
type Metrics interface {
	// RecordRpcResponse call-back method that includes rpc method, response code, and duration.
	// ctx carries the per-RPC OpenTelemetry span (the gRPC client is otelgrpc-instrumented), so
	// implementations can log its trace id to correlate the call back to the workflow that issued it.
	RecordRpcResponse(ctx context.Context, method, code string, duration time.Duration)
}

// TraceIDFromContext returns the hex-encoded OpenTelemetry trace id carried by ctx, or "" when ctx
// has no valid span context. It never panics, so callers may use it unconditionally regardless of
// whether the RPC originated inside a traced workflow.
func TraceIDFromContext(ctx context.Context) string {
	if sc := trace.SpanContextFromContext(ctx); sc.HasTraceID() {
		return sc.TraceID().String()
	}
	return ""
}

func newGrpcUnaryMetricsInterceptor(metrics Metrics) grpc.UnaryClientInterceptor {
	return func(ctx context.Context, method string, req interface{}, reply interface{}, cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
		var code codes.Code

		defer func(startTime time.Time) {
			metrics.RecordRpcResponse(ctx, method, normalizeRPCCode(code), time.Since(startTime))
		}(time.Now())

		err := invoker(ctx, method, req, reply, cc, opts...)
		code = status.Code(err)
		return err
	}
}

func newGrpcStreamMetricsInterceptor(metrics Metrics) grpc.StreamClientInterceptor {
	return func(ctx context.Context, desc *grpc.StreamDesc, cc *grpc.ClientConn, method string, streamer grpc.Streamer, opts ...grpc.CallOption) (grpc.ClientStream, error) {
		var code codes.Code

		defer func(startTime time.Time) {
			metrics.RecordRpcResponse(ctx, method, normalizeRPCCode(code), time.Since(startTime))
		}(time.Now())

		s, err := streamer(ctx, desc, cc, method, opts...)
		code = status.Code(err)
		return s, err
	}
}

// to match nico gRPC status code, which is translated as Ok, instead of go translation of OK
func normalizeRPCCode(code codes.Code) string {
	if code == codes.OK {
		return "Ok"
	}
	return code.String()
}
