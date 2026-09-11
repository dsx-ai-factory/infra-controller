// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tracer

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/codes"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
)

func TestCurrentContextSpanEndWith(t *testing.T) {
	tests := []struct {
		name            string
		err             error
		wantStatus      codes.Code
		wantDescription string
	}{
		{name: "success", wantStatus: codes.Unset},
		{name: "error", err: errors.New("database operation failed"), wantStatus: codes.Error, wantDescription: "database operation failed"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			recorder := tracetest.NewSpanRecorder()
			tp := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
			previous := otel.GetTracerProvider()
			otel.SetTracerProvider(tp)
			t.Cleanup(func() { otel.SetTracerProvider(previous) })

			_, span := NewTracerSpan().CreateChildInCurrentContext(context.Background(), "DAO.Operation")
			span.EndWith(tt.err)

			ended := recorder.Ended()
			assert.Len(t, ended, 1)
			assert.Equal(t, tt.wantStatus, ended[0].Status().Code)
			assert.Equal(t, tt.wantDescription, ended[0].Status().Description)
		})
	}
}

func Test_SetSpanAttribute(t *testing.T) {
	type args struct {
		inputAttributeKey   string
		inputAttributeValue string
		inputSpan           *CurrentContextSpan
		expectSpan          *CurrentContextSpan
	}

	// OTEL Spanner configuration
	provider := trace.NewNoopTracerProvider()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})

	ctx := trace.ContextWithRemoteSpanContext(context.Background(), sc)
	tracer := provider.Tracer("Test_SetSpanAttribute")
	_, validspan := tracer.Start(ctx, "Test_SetSpanAttribute")

	tracerSpan := NewTracerSpan()

	tests := []struct {
		name string
		args args
	}{
		{
			name: "test set span attribute success returns valid span",
			args: args{
				inputAttributeKey:   "test",
				inputAttributeValue: "test",
				inputSpan: &CurrentContextSpan{
					Span: validspan,
				},
				expectSpan: &CurrentContextSpan{
					Span: validspan,
				},
			},
		},
		{
			name: "test set span attribute success returns nil span",
			args: args{
				inputAttributeKey:   "test",
				inputAttributeValue: "test",
				inputSpan:           nil,
				expectSpan:          nil,
			},
		},
		{
			name: "test set span attribute success returns nil in case value empty",
			args: args{
				inputAttributeKey:   "test",
				inputAttributeValue: "",
				inputSpan: &CurrentContextSpan{
					Span: validspan,
				},
				expectSpan: &CurrentContextSpan{
					Span: validspan,
				},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			span := tracerSpan.SetAttribute(tt.args.inputSpan, tt.args.inputAttributeKey, tt.args.inputAttributeValue)
			assert.Equal(t, span, tt.args.expectSpan)
		})
	}
}

func Test_CreateChildInCurrentContext(t *testing.T) {
	// Install a recording provider as the global, since spans now come from
	// the global TracerProvider rather than a tracer stored in the context.
	recorder := tracetest.NewSpanRecorder()
	tp := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	prevTP := otel.GetTracerProvider()
	otel.SetTracerProvider(tp)
	defer otel.SetTracerProvider(prevTP)

	parentTraceID := trace.TraceID{0x01}
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID:    parentTraceID,
		SpanID:     trace.SpanID{0x01},
		TraceFlags: trace.FlagsSampled,
	})
	parentCtx := trace.ContextWithRemoteSpanContext(context.Background(), sc)

	tracerSpan := NewTracerSpan()
	var nilCtx context.Context

	tests := []struct {
		name       string
		ctx        context.Context
		spanName   string
		expectSpan bool
	}{
		{
			// DAO spans must work on any code path (Temporal workers, gRPC,
			// auth), without relying on transport-specific context values.
			name:       "test child span creation success without tracerKey in context",
			ctx:        parentCtx,
			spanName:   "test",
			expectSpan: true,
		},
		{
			name:       "test child span creation failure, empty span name",
			ctx:        parentCtx,
			spanName:   "",
			expectSpan: false,
		},
		{
			name:       "test child span creation failure, nil context",
			ctx:        nilCtx,
			spanName:   "test",
			expectSpan: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, span := tracerSpan.CreateChildInCurrentContext(tt.ctx, tt.spanName)
			if !tt.expectSpan {
				assert.Nil(t, span)
				return
			}
			assert.NotNil(t, span)
			assert.Equal(t, parentTraceID, span.Span.SpanContext().TraceID(), "child span should join the parent trace")
			span.End()
		})
	}
}
