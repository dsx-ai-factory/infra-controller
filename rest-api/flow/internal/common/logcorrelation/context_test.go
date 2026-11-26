// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package logcorrelation

import (
	"bytes"
	"context"
	"encoding/json"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc/metadata"
)

func TestCapture(t *testing.T) {
	const id = "1234567890abcdef1234567890abcdef"
	for _, tc := range []struct {
		name string
		ctx  context.Context
		want string
	}{
		{"explicit", metadata.NewIncomingContext(context.Background(), metadata.Pairs(Header, id)), id},
		{"traceparent without exporter", metadata.NewIncomingContext(context.Background(), metadata.Pairs("traceparent", "00-"+id+"-1234567890abcdef-00")), id},
		{"missing", context.Background(), ""},
		{"invalid", metadata.NewIncomingContext(context.Background(), metadata.Pairs(Header, "invalid", "traceparent", "invalid")), ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := Capture(tc.ctx)
			require.True(t, valid(got))
			if tc.want != "" {
				require.Equal(t, tc.want, got)
			}
		})
	}
}

func TestRestore(t *testing.T) {
	const id = "1234567890abcdef1234567890abcdef"
	background := trace.NewSpanContext(trace.SpanContextConfig{TraceID: trace.TraceID{1}, SpanID: trace.SpanID{2}})
	parent := trace.ContextWithSpanContext(context.Background(), background)
	parent = metadata.NewOutgoingContext(parent, metadata.Pairs("authorization", "test-fixture"))
	for _, value := range []string{id, "", "invalid"} {
		t.Run(value, func(t *testing.T) {
			ctx := Restore(parent, value)
			require.Equal(t, background, trace.SpanContextFromContext(ctx))
			md, _ := metadata.FromOutgoingContext(ctx)
			require.Equal(t, []string{"test-fixture"}, md.Get("authorization"))
			original, _ := metadata.FromOutgoingContext(parent)
			require.Empty(t, original.Get(Header))
			if value == id {
				require.Equal(t, []string{id}, md.Get(Header))
				var output bytes.Buffer
				logger := LoggerWithTrace(ctx, zerolog.New(&output))
				logger.Info().Msg("restored")
				var event map[string]any
				require.NoError(t, json.Unmarshal(output.Bytes(), &event))
				require.Equal(t, id, event["trace_id"])
				require.Equal(t, background.SpanID().String(), event["span_id"])
			} else {
				require.Empty(t, md.Get(Header))
			}
		})
	}
}
