// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpclog

import (
	"context"
	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/status"
)

// UnaryServerInterceptor returns a gRPC unary server interceptor that emits a
// single structured log line per RPC on completion. Fields:
//
//   - grpc.service     service name parsed from the full method
//   - grpc.method      method name parsed from the full method
//   - grpc.code        status code as a string ("OK", "Unavailable", ...)
//   - grpc.duration_ms wall-clock latency in milliseconds
//   - grpc.peer        client address from the peer context, when available
//   - grpc.error       status message, only present on failure
//
// The log level is derived from the status code via LevelFromCode.
// A valid context also supplies trace_id/span_id, including unsampled traces.
func UnaryServerInterceptor() grpc.UnaryServerInterceptor {
	return func(
		ctx context.Context,
		req any,
		info *grpc.UnaryServerInfo,
		handler grpc.UnaryHandler,
	) (any, error) {
		start := time.Now()
		resp, err := handler(ctx, req)
		durMs := time.Since(start).Milliseconds()

		st, _ := status.FromError(err)
		service, method := splitFullMethod(info.FullMethod)

		logger := cotel.LoggerWithTrace(ctx, log.Logger)
		ev := logger.WithLevel(LevelFromCode(st.Code())).
			Str("grpc.service", service).
			Str("grpc.method", method).
			Str("grpc.code", st.Code().String()).
			Int64("grpc.duration_ms", durMs)

		if p, ok := peer.FromContext(ctx); ok && p.Addr != nil {
			ev = ev.Str("grpc.peer", p.Addr.String())
		}
		if err != nil {
			ev = ev.Str("grpc.error", st.Message())
		}
		ev.Msg("flow rpc handled")

		return resp, err
	}
}
