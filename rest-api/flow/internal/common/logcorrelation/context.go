// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package logcorrelation carries durable log trace IDs without creating spans.
package logcorrelation

import (
	"context"
	"strings"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc/metadata"
)

// Header transports log correlation independently of W3C tracing context.
const Header = "x-nico-trace-id"

type contextKey struct{}

// Metadata stores the originating log trace with a durable operation record.
type Metadata struct {
	TraceID string `json:"trace_id"`
}

// Current returns a restored log ID, or the valid active OTel trace ID.
func Current(ctx context.Context) string {
	if id, ok := ctx.Value(contextKey{}).(string); ok {
		return id
	}
	if span := trace.SpanContextFromContext(ctx); span.IsValid() {
		return span.TraceID().String()
	}
	return ""
}

// Capture reuses incoming correlation or trace context, generating a log-only ID
// when neither exists. Persist the result once with the operation or task.
func Capture(ctx context.Context) string {
	if id, ok := ctx.Value(contextKey{}).(string); ok {
		return id
	}
	if md, ok := metadata.FromIncomingContext(ctx); ok {
		if values := md.Get(Header); len(values) == 1 && valid(values[0]) {
			return values[0]
		}
		if values := md.Get("traceparent"); len(values) == 1 {
			extracted := propagation.TraceContext{}.Extract(context.Background(), propagation.MapCarrier{"traceparent": values[0]})
			if span := trace.SpanContextFromContext(extracted); span.IsValid() {
				return span.TraceID().String()
			}
		}
	}
	if id := Current(ctx); id != "" {
		return id
	}
	return strings.ReplaceAll(uuid.NewString(), "-", "")
}

// LoggerWithTrace prefers the restored originating ID and retains the active
// span ID when present. Ordinary requests retain existing OTel logging behavior.
func LoggerWithTrace(ctx context.Context, base zerolog.Logger) zerolog.Logger {
	id, ok := ctx.Value(contextKey{}).(string)
	if !ok {
		return cotel.LoggerWithTrace(ctx, base)
	}
	fields := base.With().Str("trace_id", id)
	if span := trace.SpanContextFromContext(ctx); span.IsValid() {
		fields = fields.Str("span_id", span.SpanID().String())
	}
	return fields.Logger()
}

// Restore attaches a persisted ID to logs and outgoing gRPC metadata. Empty or
// invalid legacy values leave the context unchanged; no span is fabricated.
func Restore(ctx context.Context, id string) context.Context {
	if !valid(id) {
		return ctx
	}
	if current, ok := ctx.Value(contextKey{}).(string); ok && current == id {
		return ctx
	}
	ctx = context.WithValue(ctx, contextKey{}, id)
	md, _ := metadata.FromOutgoingContext(ctx)
	md = md.Copy()
	md.Set(Header, id)
	ctx = metadata.NewOutgoingContext(ctx, md)
	logger := log.Ctx(ctx).With().Str("trace_id", id).Logger()
	return logger.WithContext(ctx)
}

func valid(id string) bool {
	parsed, err := trace.TraceIDFromHex(id)
	return err == nil && parsed.IsValid() && parsed.String() == id
}
