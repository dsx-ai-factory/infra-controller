// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpclog

import (
	"context"
	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc"
	"google.golang.org/grpc/status"
)

// UnaryClientInterceptor returns a gRPC unary client interceptor that emits a
// single structured log line per outgoing RPC on completion. Fields:
//
//   - grpc.target       caller-supplied label identifying the remote, e.g.
//     "nico-core-api". Distinguishes logs when a binary speaks to multiple
//     gRPC servers.
//   - grpc.service      service name parsed from the full method
//   - grpc.method       method name parsed from the full method
//   - grpc.code         status code as a string
//   - grpc.duration_ms  wall-clock latency in milliseconds
//   - grpc.error        status message, only present on failure
//
// The log level is derived from the status code via LevelFromCode.
// A valid context also supplies trace_id/span_id, including unsampled traces.
//
// target is a human-friendly label, not the dial address: a stable
// non-cardinal value (e.g. "nico-core-api") is much easier to grep than a
// service-discovered DNS name that changes per pod.
func UnaryClientInterceptor(target string) grpc.UnaryClientInterceptor {
	return func(
		ctx context.Context,
		fullMethod string,
		req, reply any,
		cc *grpc.ClientConn,
		invoker grpc.UnaryInvoker,
		opts ...grpc.CallOption,
	) error {
		start := time.Now()
		err := invoker(ctx, fullMethod, req, reply, cc, opts...)
		durMs := time.Since(start).Milliseconds()

		st, _ := status.FromError(err)
		service, method := splitFullMethod(fullMethod)

		logger := cotel.LoggerWithTrace(ctx, log.Logger)
		ev := logger.WithLevel(LevelFromCode(st.Code())).
			Str("grpc.target", target).
			Str("grpc.service", service).
			Str("grpc.method", method).
			Str("grpc.code", st.Code().String()).
			Int64("grpc.duration_ms", durMs)

		if err != nil {
			ev = ev.Str("grpc.error", st.Message())
		}
		ev.Msg("grpc client call")

		return err
	}
}
