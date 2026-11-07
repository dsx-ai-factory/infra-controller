// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc/metadata"
)

func TestInjectOutgoingTraceContext(t *testing.T) {
	previousPropagator := otel.GetTextMapPropagator()
	otel.SetTextMapPropagator(propagation.TraceContext{})
	t.Cleanup(func() { otel.SetTextMapPropagator(previousPropagator) })

	spanContext := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID:    trace.TraceID{1},
		SpanID:     trace.SpanID{2},
		TraceFlags: trace.FlagsSampled,
	})
	tests := []struct {
		name     string
		metadata metadata.MD
	}{
		{name: "adds trace context", metadata: metadata.Pairs("existing", "value")},
		{name: "replaces stale trace context", metadata: metadata.Pairs("traceparent", "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-00")},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			ctx := trace.ContextWithSpanContext(context.Background(), spanContext)
			ctx = metadata.NewOutgoingContext(ctx, test.metadata)

			md, ok := metadata.FromOutgoingContext(injectOutgoingTraceContext(ctx))
			require.True(t, ok)
			assert.Equal(t, []string{"00-01000000000000000000000000000000-0200000000000000-01"}, md.Get("traceparent"))
			if test.metadata.Get("existing") != nil {
				assert.Equal(t, []string{"value"}, md.Get("existing"))
			}
		})
	}
}
