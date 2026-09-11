// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package otel

import (
	"context"
	"net/http"

	"github.com/rs/zerolog"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/trace"
)

// tracerName identifies spans created through this package's helpers.
const tracerName = "github.com/NVIDIA/infra-controller/rest-api"

// StartSpan starts a child span using the global TracerProvider; the parent is
// taken from ctx via its W3C span context, so it works under otelecho, Temporal
// workers, gRPC handlers — anywhere a context flows. It never returns a nil
// span: when no provider is installed the returned span is a no-op.
func StartSpan(ctx context.Context, name string, attrs ...attribute.KeyValue) (context.Context, trace.Span) {
	var opts []trace.SpanStartOption
	if len(attrs) > 0 {
		opts = append(opts, trace.WithAttributes(attrs...))
	}
	return otel.Tracer(tracerName).Start(ctx, name, opts...)
}

// EndSpan records an error when err is non-nil and ends the span. Intended for
// use with named returns:
// defer func() { cotel.EndSpan(span, retErr) }().
func EndSpan(span trace.Span, err error) {
	if span == nil {
		return
	}
	if err != nil {
		RecordError(span, err)
	}
	span.End()
}

// RecordError records err and marks the span as failed without ending it.
func RecordError(span trace.Span, err error) {
	if span == nil || err == nil {
		return
	}
	span.RecordError(err)
	span.SetStatus(codes.Error, err.Error())
}

// RecordHTTPError records stable HTTP failure metadata without exporting
// response messages or internal error details. Server failures mark the span
// as failed; client failures are recorded as events.
func RecordHTTPError(ctx context.Context, statusCode int) {
	span := trace.SpanFromContext(ctx)
	if statusCode >= http.StatusInternalServerError {
		span.SetStatus(codes.Error, "")
		span.SetAttributes(
			attribute.Int("http.response.status_code", statusCode),
			attribute.String("error.type", "api.internal"),
		)
	} else if statusCode >= http.StatusBadRequest {
		span.AddEvent("api.error", trace.WithAttributes(
			attribute.Int("http.response.status_code", statusCode),
		))
	}
}

// RecordSuccess marks a span as successfully completed.
func RecordSuccess(span trace.Span) {
	if span != nil {
		span.SetStatus(codes.Ok, "")
	}
}

// SetAttribute sets an attribute on the span.
func SetAttribute(span trace.Span, attr attribute.KeyValue) {
	if span == nil || !attr.Valid() {
		return
	}
	span.SetAttributes(attr)
}

// LoggerWithTrace returns logger enriched with trace_id/span_id fields when
// ctx carries a valid span context, enabling log/trace correlation.
func LoggerWithTrace(ctx context.Context, logger zerolog.Logger) zerolog.Logger {
	sc := trace.SpanContextFromContext(ctx)
	if !sc.HasTraceID() {
		return logger
	}
	lctx := logger.With().Str("trace_id", sc.TraceID().String())
	if sc.HasSpanID() {
		lctx = lctx.Str("span_id", sc.SpanID().String())
	}
	return lctx.Logger()
}
