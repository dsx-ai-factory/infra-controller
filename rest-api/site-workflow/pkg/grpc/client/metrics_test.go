// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"go.opentelemetry.io/otel/trace"
)

func TestTraceIDFromContext(t *testing.T) {
	traceID := trace.TraceID{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10}

	tcs := []struct {
		descr string
		ctx   context.Context
		want  string
	}{
		{
			descr: "no span context returns empty",
			ctx:   context.Background(),
			want:  "",
		},
		{
			descr: "valid span context returns hex-encoded trace id",
			ctx: trace.ContextWithSpanContext(context.Background(), trace.NewSpanContext(trace.SpanContextConfig{
				TraceID: traceID,
			})),
			want: "0102030405060708090a0b0c0d0e0f10",
		},
		{
			descr: "all-zero trace id is treated as absent",
			ctx: trace.ContextWithSpanContext(context.Background(), trace.NewSpanContext(trace.SpanContextConfig{
				TraceID: trace.TraceID{},
			})),
			want: "",
		},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			assert.Equal(t, tc.want, TraceIDFromContext(tc.ctx))
		})
	}
}
