// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"context"
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/trace"
	oteltrace "go.opentelemetry.io/otel/trace"
)

func Test_LoadFromContext(t *testing.T) {
	type args struct {
		ctx             context.Context
		expectValidSpan bool
		expectSpan      oteltrace.Span
	}

	// OTEL Spanner configuration
	provider := trace.NewNoopTracerProvider()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})

	ctx := trace.ContextWithRemoteSpanContext(context.Background(), sc)
	tracer := provider.Tracer("Test_LoadFromContext")
	validspanctx, validspan := tracer.Start(ctx, "Test_LoadFromContext")

	tracerSpan := NewTracerSpan()

	tests := []struct {
		name string
		args args
	}{
		{
			name: "test load span success in case span information not present in context",
			args: args{
				ctx:             context.Background(),
				expectValidSpan: false,
				expectSpan:      nil,
			},
		},
		{
			name: "test load span success in case span information present in context",
			args: args{
				ctx:             validspanctx,
				expectValidSpan: true,
				expectSpan:      validspan,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			span, valid := tracerSpan.LoadFromContext(tt.args.ctx)
			assert.Equal(t, valid, tt.args.expectValidSpan)
			assert.Equal(t, span, tt.args.expectSpan)
		})
	}
}

func Test_SetAttribute(t *testing.T) {
	type args struct {
		inputKeyValue attribute.KeyValue
		inputLogger   zerolog.Logger
		inputSpan     oteltrace.Span
		expectSpan    oteltrace.Span
	}

	// OTEL Spanner configuration
	provider := trace.NewNoopTracerProvider()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})

	ctx := trace.ContextWithRemoteSpanContext(context.Background(), sc)
	tracer := provider.Tracer("Test_SetAttribute")
	_, validspan := tracer.Start(ctx, "Test_SetAttribute")

	tracerSpan := NewTracerSpan()

	tests := []struct {
		name string
		args args
	}{
		{
			name: "test set span attribute success valid span and keyvalue returns valid span",
			args: args{
				inputKeyValue: attribute.String("test", "test"),
				inputSpan:     validspan,
				expectSpan:    validspan,
			},
		},
		{
			name: "test set span attribute success empty span and valid keyvalue returns nil span",
			args: args{
				inputKeyValue: attribute.String("test", "test"),
				inputSpan:     nil,
				expectSpan:    nil,
			},
		},
		{
			name: "test set span attribute success returns nil in case keyvalue invalid",
			args: args{
				inputSpan:  validspan,
				expectSpan: validspan,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			span := tracerSpan.SetAttribute(tt.args.inputSpan, tt.args.inputKeyValue, tt.args.inputLogger)
			assert.Equal(t, span, tt.args.expectSpan)
		})
	}
}

func Test_CreateChildInContext(t *testing.T) {
	type args struct {
		inputSpanName string
		inputCtx      context.Context
		inputLogger   zerolog.Logger
		expectSpan    oteltrace.Span
	}

	// OTEL Spanner configuration
	provider := trace.NewNoopTracerProvider()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})

	ctx1 := trace.ContextWithRemoteSpanContext(context.Background(), sc)
	tracer := provider.Tracer("Test_CreateChildInContext")
	_, validspan := tracer.Start(ctx1, "Test_CreateChildInContext")

	ctx2 := ctx1

	// Set parent tracer in current context
	ctx1 = context.WithValue(ctx1, otelecho.TracerKey, tracer)
	tracerSpan := NewTracerSpan()

	var ctx3 context.Context

	tests := []struct {
		name string
		args args
	}{
		{
			name: "test child span creation in context failure, no tracerKey presents",
			args: args{
				inputCtx:      ctx2,
				inputSpanName: "test",
				expectSpan:    nil,
			},
		},
		{
			name: "test child span creation in context failure, empty span name",
			args: args{
				inputCtx:      ctx2,
				inputSpanName: "",
				expectSpan:    nil,
			},
		},
		{
			name: "test child span creation in context failure, empty context",
			args: args{
				inputCtx:      ctx3,
				inputSpanName: "",
				expectSpan:    nil,
			},
		},
		{
			name: "test child span creation success",
			args: args{
				inputCtx:      ctx1,
				inputSpanName: "test",
				expectSpan:    validspan,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, span := tracerSpan.CreateChildInContext(tt.args.inputCtx, tt.args.inputSpanName, tt.args.inputLogger)
			assert.Equal(t, span, tt.args.expectSpan)
		})
	}

	t.Run("missing tracer is a debug-level no-op", func(t *testing.T) {
		var logOutput bytes.Buffer
		logger := zerolog.New(&logOutput)

		_, span := tracerSpan.CreateChildInContext(ctx2, "test", logger)

		assert.Nil(t, span)
		var logEntry map[string]interface{}
		require.NoError(t, json.Unmarshal(logOutput.Bytes(), &logEntry))
		assert.Equal(t, zerolog.LevelDebugValue, logEntry[zerolog.LevelFieldName])
	})
}
