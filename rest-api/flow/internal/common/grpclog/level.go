// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package grpclog provides zerolog-based unary interceptors for the Flow gRPC
// server and clients. Each interceptor emits exactly one log line per RPC on
// completion with the method, duration, and status code. Request and response
// payloads are intentionally not logged: several NICo / Flow RPCs carry BMC
// credentials, and full protobuf dumps make the log unreadable.
package grpclog

import (
	"strings"

	"github.com/rs/zerolog"
	"google.golang.org/grpc/codes"
)

// LevelFromCode maps a gRPC status code to the zerolog level used when
// reporting an RPC's outcome. The mapping is intentionally shared between the
// server and client interceptors so a given code logs at the same level no
// matter which side observes it.
//
// The classification is:
//
//   - Info  for OK and expected business-level outcomes that callers routinely
//     branch on (NotFound, AlreadyExists, InvalidArgument, …). These show up
//     during normal sync flows and should not raise alarm.
//   - Warn  for outcomes that are usually transient but worth surfacing
//     (DeadlineExceeded, ResourceExhausted, Aborted).
//   - Error for outcomes that indicate the remote is broken
//     (Unavailable, Internal, DataLoss, Unknown).
//
// Canceled is logged at Info because it is overwhelmingly produced by graceful
// shutdown or a caller giving up, not by a real failure.
func LevelFromCode(code codes.Code) zerolog.Level {
	switch code {
	case codes.OK,
		codes.Canceled,
		codes.InvalidArgument,
		codes.NotFound,
		codes.AlreadyExists,
		codes.PermissionDenied,
		codes.Unauthenticated,
		codes.FailedPrecondition,
		codes.OutOfRange,
		codes.Unimplemented:
		return zerolog.InfoLevel
	case codes.DeadlineExceeded,
		codes.ResourceExhausted,
		codes.Aborted:
		return zerolog.WarnLevel
	default:
		// Unknown, Internal, Unavailable, DataLoss, and any future codes
		// default to Error: better to over-alert on a new code than to hide
		// a real outage behind Info.
		return zerolog.ErrorLevel
	}
}

// splitFullMethod splits "/forge.Forge/GetMachines" into ("forge.Forge",
// "GetMachines"). Inputs that do not contain a service / method separator are
// returned as ("", trimmed) so the method field is still populated.
func splitFullMethod(full string) (service, method string) {
	trimmed := strings.TrimPrefix(full, "/")
	if i := strings.LastIndexByte(trimmed, '/'); i >= 0 {
		return trimmed[:i], trimmed[i+1:]
	}
	return "", trimmed
}
