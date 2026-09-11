// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tracer

import (
	"context"

	"go.opentelemetry.io/otel/attribute"
	oteltrace "go.opentelemetry.io/otel/trace"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// CurrentContextSpan adapts the shared OTel span API to the existing DAO
// method signatures.
type CurrentContextSpan struct {
	Span oteltrace.Span
}

// End stop the span from leakage
func (c *CurrentContextSpan) End() {
	c.Span.End()
}

// EndWith records err on the span (when non-nil) and ends it. Intended for
// named-return defers: defer func() { daoSpan.EndWith(retErr) }().
func (c *CurrentContextSpan) EndWith(err error) {
	cotel.EndSpan(c.Span, err)
}

// TracerSpan adapts shared span operations to existing DAO fields.
type TracerSpan struct {
}

func NewTracerSpan() *TracerSpan {
	return &TracerSpan{}
}

// SetAttribute set key value attribute to current span
func (c *TracerSpan) SetAttribute(cspan *CurrentContextSpan, key string, value interface{}) *CurrentContextSpan {
	if cspan == nil {
		return cspan
	}

	if value == "" {
		return cspan
	}

	svalue, ok := value.(string)
	if ok {
		cotel.SetAttribute(cspan.Span, attribute.String(key, svalue))
	}

	return cspan
}

// CreateChildInCurrentContext create a child span from specified span name and
// context. The span comes from the global TracerProvider with its parent taken
// from ctx, so DAO spans nest correctly on any code path (HTTP, Temporal
// workers, gRPC) — not only under the echo middleware.
func (c *TracerSpan) CreateChildInCurrentContext(ctx context.Context, spanName string) (context.Context, *CurrentContextSpan) {
	// Check if given context is empty
	if ctx == nil {
		return ctx, nil
	}

	if spanName == "" {
		return ctx, nil
	}

	// create a child span in current context
	newctx, span := cotel.StartSpan(ctx, spanName)
	return newctx, &CurrentContextSpan{
		Span: span,
	}
}
