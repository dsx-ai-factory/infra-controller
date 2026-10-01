// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package otel

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
)

func installTestTracerProvider(t *testing.T) *tracetest.SpanRecorder {
	t.Helper()
	recorder := tracetest.NewSpanRecorder()
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	previous := otel.GetTracerProvider()
	otel.SetTracerProvider(provider)
	t.Cleanup(func() { otel.SetTracerProvider(previous) })
	return recorder
}

func spanAttributes(span sdktrace.ReadOnlySpan) map[string]string {
	attributes := make(map[string]string, len(span.Attributes()))
	for _, attr := range span.Attributes() {
		attributes[string(attr.Key)] = attr.Value.String()
	}
	return attributes
}

func TestStartSpanPreservesParentAndAttributes(t *testing.T) {
	recorder := installTestTracerProvider(t)
	parentTraceID := trace.TraceID{0x01}
	parent := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID:    parentTraceID,
		SpanID:     trace.SpanID{0x01},
		TraceFlags: trace.FlagsSampled,
		Remote:     true,
	})
	parentCtx := trace.ContextWithRemoteSpanContext(context.Background(), parent)

	ctx, span := StartSpan(
		parentCtx,
		"child",
		attribute.String("registration_token", "super-secret"),
		attribute.String("site_id", "site-123"),
	)

	assert.Equal(t, parentTraceID, trace.SpanContextFromContext(ctx).TraceID())
	span.End()
	ended := recorder.Ended()
	require.Len(t, ended, 1)
	attrs := spanAttributes(ended[0])
	assert.Equal(t, "super-secret", attrs["registration_token"])
	assert.Equal(t, "site-123", attrs["site_id"])
}

func TestSetAttribute(t *testing.T) {
	recorder := installTestTracerProvider(t)
	_, span := StartSpan(context.Background(), "attributes")
	SetAttribute(span, attribute.String("site_id", "site-123"))
	SetAttribute(span, attribute.KeyValue{})
	SetAttribute(nil, attribute.String("ignored", "value"))
	span.End()

	ended := recorder.Ended()
	require.Len(t, ended, 1)
	attrs := spanAttributes(ended[0])
	assert.Equal(t, "site-123", attrs["site_id"])
}

func TestRecordHTTPError(t *testing.T) {
	tests := []struct {
		descr       string
		statusCode  int
		wantStatus  codes.Code
		wantEvents  int
		wantErrorTy string
	}{
		{descr: "success is unchanged", statusCode: 399, wantStatus: codes.Unset},
		{descr: "client error is an event", statusCode: 400, wantStatus: codes.Unset, wantEvents: 1},
		{descr: "server error marks span", statusCode: 500, wantStatus: codes.Error, wantErrorTy: "api.internal"},
	}

	for _, tc := range tests {
		t.Run(tc.descr, func(t *testing.T) {
			recorder := installTestTracerProvider(t)
			ctx, span := StartSpan(context.Background(), "request")
			RecordHTTPError(ctx, tc.statusCode)
			span.End()

			ended := recorder.Ended()
			require.Len(t, ended, 1)
			assert.Equal(t, tc.wantStatus, ended[0].Status().Code)
			assert.Len(t, ended[0].Events(), tc.wantEvents)
			attrs := spanAttributes(ended[0])
			assert.Equal(t, tc.wantErrorTy, attrs["error.type"])
		})
	}
}

func TestRecordSuccess(t *testing.T) {
	recorder := installTestTracerProvider(t)
	_, span := StartSpan(context.Background(), "operation")
	RecordSuccess(span)
	RecordSuccess(nil)
	span.End()

	ended := recorder.Ended()
	require.Len(t, ended, 1)
	assert.Equal(t, codes.Ok, ended[0].Status().Code)
}

func TestErrorHelpersRecordErrors(t *testing.T) {
	tcs := []struct {
		descr string
		mark  func(span sdktrace.ReadWriteSpan, err error)
	}{
		{
			descr: "record error",
			mark: func(span sdktrace.ReadWriteSpan, err error) {
				RecordError(span, err)
				span.End()
			},
		},
		{
			descr: "end span",
			mark: func(span sdktrace.ReadWriteSpan, err error) {
				EndSpan(span, err)
			},
		},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			recorder := tracetest.NewSpanRecorder()
			provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
			_, span := provider.Tracer("test").Start(context.Background(), "operation")
			readWriteSpan, ok := span.(sdktrace.ReadWriteSpan)
			require.True(t, ok)

			const message = "operation failed"
			tc.mark(readWriteSpan, errors.New(message))

			ended := recorder.Ended()
			require.Len(t, ended, 1)
			assert.Equal(t, codes.Error, ended[0].Status().Code)
			assert.Equal(t, message, ended[0].Status().Description)
			require.Len(t, ended[0].Events(), 1)
			assert.Equal(t, "exception", ended[0].Events()[0].Name)
		})
	}
}
