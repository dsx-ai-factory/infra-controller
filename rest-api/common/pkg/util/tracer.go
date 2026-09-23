// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	"github.com/rs/zerolog"
	"go.opentelemetry.io/otel/attribute"
	oteltrace "go.opentelemetry.io/otel/trace"
)

// TracerSpan holds span information
type TracerSpan struct {
}

func NewTracerSpan() *TracerSpan {
	return &TracerSpan{}
}

// LoadFromContext validate and get the spanner from current context
func (c *TracerSpan) LoadFromContext(ctx context.Context) (oteltrace.Span, bool) {
	// Assert we don't have a span on the context.
	span := oteltrace.SpanFromContext(ctx)
	if span.SpanContext().IsValid() {
		return span, true
	}
	return nil, false
}

// SetAttribute set key value attribute to current span
func (c *TracerSpan) SetAttribute(cspan oteltrace.Span, kv attribute.KeyValue, logger zerolog.Logger) oteltrace.Span {

	if cspan == nil {
		logger.Warn().Msg("error setting span attribute, span is nil")
		return cspan
	}

	if !kv.Valid() {
		logger.Warn().Msg("error setting span attribute, keyvalue is invalid")
		return cspan
	}

	if cspan.SpanContext().IsValid() {
		cspan.SetAttributes(kv)
	} else {
		logger.Error().Msg("error setting span attribute, span context is invalid")
	}

	return cspan
}

// CreateChildInContext create a child span from specified span name and context
func (c *TracerSpan) CreateChildInContext(ctx context.Context, spanName string, logger zerolog.Logger) (context.Context, oteltrace.Span) {

	// check if given context is empty
	if ctx == nil {
		logger.Warn().Msg("input context is nil, can't create child spanner")
		return ctx, nil
	}

	if spanName == "" {
		logger.Warn().Msg("spanner name is empty, can't create child spanner")
		return ctx, nil
	}

	// get root tracer from context
	tracer, ok := ctx.Value(otelecho.TracerKey).(oteltrace.Tracer)
	if !ok {
		logger.Debug().Msg("error extracting tracer from context")
		return ctx, nil
	}

	// create a child span in current context
	return tracer.Start(ctx, spanName)
}
