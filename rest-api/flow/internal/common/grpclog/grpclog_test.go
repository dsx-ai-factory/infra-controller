// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpclog

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"testing"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/status"
)

func TestLevelFromCode(t *testing.T) {
	cases := []struct {
		code codes.Code
		want zerolog.Level
	}{
		{codes.OK, zerolog.InfoLevel},
		{codes.Canceled, zerolog.InfoLevel},
		{codes.InvalidArgument, zerolog.InfoLevel},
		{codes.NotFound, zerolog.InfoLevel},
		{codes.AlreadyExists, zerolog.InfoLevel},
		{codes.PermissionDenied, zerolog.InfoLevel},
		{codes.Unauthenticated, zerolog.InfoLevel},
		{codes.FailedPrecondition, zerolog.InfoLevel},
		{codes.OutOfRange, zerolog.InfoLevel},
		{codes.Unimplemented, zerolog.InfoLevel},

		{codes.DeadlineExceeded, zerolog.WarnLevel},
		{codes.ResourceExhausted, zerolog.WarnLevel},
		{codes.Aborted, zerolog.WarnLevel},

		{codes.Unknown, zerolog.ErrorLevel},
		{codes.Internal, zerolog.ErrorLevel},
		{codes.Unavailable, zerolog.ErrorLevel},
		{codes.DataLoss, zerolog.ErrorLevel},
	}
	for _, tc := range cases {
		t.Run(tc.code.String(), func(t *testing.T) {
			assert.Equal(t, tc.want, LevelFromCode(tc.code))
		})
	}
}

func TestSplitFullMethod(t *testing.T) {
	cases := []struct {
		in          string
		wantService string
		wantMethod  string
	}{
		{"/forge.Forge/GetMachines", "forge.Forge", "GetMachines"},
		{"/flow.v1.Flow/GetRackInfoByID", "flow.v1.Flow", "GetRackInfoByID"},
		{"", "", ""},
		{"NoSlash", "", "NoSlash"},
		{"/JustOneSegment", "", "JustOneSegment"},
	}
	for _, tc := range cases {
		t.Run(tc.in, func(t *testing.T) {
			gotSvc, gotMethod := splitFullMethod(tc.in)
			assert.Equal(t, tc.wantService, gotSvc)
			assert.Equal(t, tc.wantMethod, gotMethod)
		})
	}
}

// captureLogs swaps the global zerolog logger to write JSON into a buffer for
// the duration of the test, then restores the previous logger. It returns the
// buffer; callers parse the line(s) themselves so each test can assert on the
// fields it cares about without a shared schema getting in the way.
func captureLogs(t *testing.T) *bytes.Buffer {
	t.Helper()
	prev := log.Logger
	buf := &bytes.Buffer{}
	log.Logger = zerolog.New(buf)
	t.Cleanup(func() { log.Logger = prev })
	return buf
}

func decodeOneLogLine(t *testing.T, buf *bytes.Buffer) map[string]any {
	t.Helper()
	require.NotZero(t, buf.Len(), "expected at least one log line")
	var line map[string]any
	require.NoError(t, json.Unmarshal(bytes.TrimSpace(buf.Bytes()), &line))
	return line
}

func TestUnaryServerInterceptor_TraceCorrelation(t *testing.T) {
	for _, tc := range []struct {
		name    string
		context trace.SpanContext
	}{
		{name: "no trace"},
		{name: "sampled", context: trace.NewSpanContext(trace.SpanContextConfig{TraceID: trace.TraceID{1}, SpanID: trace.SpanID{2}, TraceFlags: trace.FlagsSampled})},
		{name: "unsampled", context: trace.NewSpanContext(trace.SpanContextConfig{TraceID: trace.TraceID{3}, SpanID: trace.SpanID{4}})},
	} {
		t.Run(tc.name, func(t *testing.T) {
			buf := captureLogs(t)
			ctx := trace.ContextWithSpanContext(context.Background(), tc.context)
			_, err := UnaryServerInterceptor()(ctx, nil, &grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Version"},
				func(context.Context, any) (any, error) { return nil, nil })
			require.NoError(t, err)
			line := decodeOneLogLine(t, buf)
			assert.Equal(t, "flow rpc handled", line["message"])
			if tc.context.IsValid() {
				assert.Equal(t, tc.context.TraceID().String(), line["trace_id"])
				assert.Equal(t, tc.context.SpanID().String(), line["span_id"])
			} else {
				assert.NotContains(t, line, "trace_id")
				assert.NotContains(t, line, "span_id")
			}
			buf.Reset()
			log.Info().Msg("outside request")
			line = decodeOneLogLine(t, buf)
			assert.NotContains(t, line, "trace_id")
			assert.NotContains(t, line, "span_id")
		})
	}
}

func TestUnaryClientInterceptor_TraceCorrelation(t *testing.T) {
	for _, tc := range []struct {
		name    string
		context trace.SpanContext
	}{
		{name: "no trace"},
		{name: "sampled", context: trace.NewSpanContext(trace.SpanContextConfig{TraceID: trace.TraceID{1}, SpanID: trace.SpanID{2}, TraceFlags: trace.FlagsSampled})},
		{name: "unsampled", context: trace.NewSpanContext(trace.SpanContextConfig{TraceID: trace.TraceID{3}, SpanID: trace.SpanID{4}})},
	} {
		t.Run(tc.name, func(t *testing.T) {
			buf := captureLogs(t)
			ctx := trace.ContextWithSpanContext(context.Background(), tc.context)
			err := UnaryClientInterceptor("nico-core-api")(ctx, "/forge.Forge/Version", nil, nil, nil,
				func(context.Context, string, any, any, *grpc.ClientConn, ...grpc.CallOption) error { return nil })
			require.NoError(t, err)
			line := decodeOneLogLine(t, buf)
			assert.Equal(t, "grpc client call", line["message"])
			if tc.context.IsValid() {
				assert.Equal(t, tc.context.TraceID().String(), line["trace_id"])
				assert.Equal(t, tc.context.SpanID().String(), line["span_id"])
			} else {
				assert.NotContains(t, line, "trace_id")
				assert.NotContains(t, line, "span_id")
			}
			buf.Reset()
			log.Info().Msg("outside request")
			line = decodeOneLogLine(t, buf)
			assert.NotContains(t, line, "trace_id")
			assert.NotContains(t, line, "span_id")
		})
	}
}

func TestUnaryServerInterceptor_Success(t *testing.T) {
	buf := captureLogs(t)

	ctx := peer.NewContext(context.Background(), &peer.Peer{
		Addr: &net.TCPAddr{IP: net.ParseIP("10.0.0.7"), Port: 4242},
	})

	interceptor := UnaryServerInterceptor()
	resp, err := interceptor(
		ctx,
		"req-payload",
		&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/GetRackInfoByID"},
		func(ctx context.Context, req any) (any, error) { return "ok-resp", nil },
	)
	require.NoError(t, err)
	assert.Equal(t, "ok-resp", resp)

	line := decodeOneLogLine(t, buf)
	assert.Equal(t, "info", line["level"])
	assert.Equal(t, "flow rpc handled", line["message"])
	assert.Equal(t, "flow.v1.Flow", line["grpc.service"])
	assert.Equal(t, "GetRackInfoByID", line["grpc.method"])
	assert.Equal(t, "OK", line["grpc.code"])
	assert.Equal(t, "10.0.0.7:4242", line["grpc.peer"])
	assert.NotContains(t, line, "grpc.error", "no error field on success")
	_, hasDur := line["grpc.duration_ms"]
	assert.True(t, hasDur, "duration_ms field present")
}

func TestUnaryServerInterceptor_Failure(t *testing.T) {
	buf := captureLogs(t)

	interceptor := UnaryServerInterceptor()
	failure := status.Error(codes.Unavailable, "core unreachable")

	_, err := interceptor(
		context.Background(),
		nil,
		&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/UpgradeFirmware"},
		func(ctx context.Context, req any) (any, error) { return nil, failure },
	)
	require.ErrorIs(t, err, failure)

	line := decodeOneLogLine(t, buf)
	assert.Equal(t, "error", line["level"])
	assert.Equal(t, "Unavailable", line["grpc.code"])
	assert.Equal(t, "core unreachable", line["grpc.error"])
	assert.NotContains(t, line, "grpc.peer", "peer field omitted when not present")
}

func TestUnaryServerInterceptor_NonStatusError(t *testing.T) {
	// A handler that returns a plain error (not a status.Status) should still
	// log a single line at Unknown / Error level.
	buf := captureLogs(t)

	interceptor := UnaryServerInterceptor()
	_, err := interceptor(
		context.Background(),
		nil,
		&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Version"},
		func(ctx context.Context, req any) (any, error) { return nil, errors.New("boom") },
	)
	require.Error(t, err)

	line := decodeOneLogLine(t, buf)
	assert.Equal(t, "error", line["level"])
	assert.Equal(t, "Unknown", line["grpc.code"])
	assert.Equal(t, "boom", line["grpc.error"])
}

func TestUnaryClientInterceptor_Success(t *testing.T) {
	buf := captureLogs(t)

	interceptor := UnaryClientInterceptor("nico-core-api")
	err := interceptor(
		context.Background(),
		"/forge.Forge/FindMachineIds",
		"req", "reply",
		nil,
		func(_ context.Context, _ string, _, _ any, _ *grpc.ClientConn, _ ...grpc.CallOption) error {
			return nil
		},
	)
	require.NoError(t, err)

	line := decodeOneLogLine(t, buf)
	assert.Equal(t, "info", line["level"])
	assert.Equal(t, "grpc client call", line["message"])
	assert.Equal(t, "nico-core-api", line["grpc.target"])
	assert.Equal(t, "forge.Forge", line["grpc.service"])
	assert.Equal(t, "FindMachineIds", line["grpc.method"])
	assert.Equal(t, "OK", line["grpc.code"])
	assert.NotContains(t, line, "grpc.error")
}

func TestUnaryClientInterceptor_Failure(t *testing.T) {
	buf := captureLogs(t)

	interceptor := UnaryClientInterceptor("nico-core-api")
	failure := status.Error(codes.DeadlineExceeded, "ctx deadline exceeded")

	err := interceptor(
		context.Background(),
		"/forge.Forge/GetPowerOptions",
		nil, nil,
		nil,
		func(_ context.Context, _ string, _, _ any, _ *grpc.ClientConn, _ ...grpc.CallOption) error {
			return failure
		},
	)
	require.ErrorIs(t, err, failure)

	line := decodeOneLogLine(t, buf)
	assert.Equal(t, "warn", line["level"])
	assert.Equal(t, "DeadlineExceeded", line["grpc.code"])
	assert.Equal(t, "ctx deadline exceeded", line["grpc.error"])
}
