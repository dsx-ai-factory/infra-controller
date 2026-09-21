// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package otelecho

import (
	"github.com/labstack/echo/v4"
	upstreamotelecho "go.opentelemetry.io/contrib/instrumentation/github.com/labstack/echo/otelecho"
	"go.opentelemetry.io/otel/trace"
)

const (
	// TracerKey is a key for current tracer
	//
	// Deprecated: spans are created from the global TracerProvider; nothing
	// reads this context key anymore.
	TracerKey = "otel-go-contrib-tracer-labstack-echo"

	// TracerName is name of the tracer
	TracerName = "go.opentelemetry.io/contrib/instrumentation/github.com/labstack/echo/otelecho"

	// TraceIDHeader carries the hex trace id of the server span on every
	// response, so a caller can quote it when reporting a failed request.
	TraceIDHeader = "X-Nico-Trace-Id"
)

// Middleware wraps the upstream otelecho middleware and ensures Echo handles a
// returned error exactly once.
func Middleware(service string, opts ...upstreamotelecho.Option) echo.MiddlewareFunc {
	upstreamMiddleware := upstreamotelecho.Middleware(service, opts...)

	return func(next echo.HandlerFunc) echo.HandlerFunc {
		// Get upstream handler
		upstreamHandler := upstreamMiddleware(func(c echo.Context) error {
			// The header has to be set before the handler writes the response,
			// and before c.Error() writes an error body below.
			if sc := trace.SpanContextFromContext(c.Request().Context()); sc.HasTraceID() {
				c.Response().Header().Set(TraceIDHeader, sc.TraceID().String())
			}

			// Note: We return the error directly and let the upstream otelecho middleware
			// handle it. The upstream middleware (v0.64.0+) will call c.Error() internally
			// to record the status code on the span. To prevent double error handling
			// (once by otelecho, once by Echo's ServeHTTP), we return nil after handling.
			err := next(c)
			if err != nil {
				// Let Echo's HTTPErrorHandler handle the error normally.
				// The upstream otelecho middleware will record the error on the span,
				// but we return the error so Echo handles it exactly once.
				// We use c.Error() here to handle it, then return nil to prevent
				// the upstream from also calling c.Error() which would double-handle.
				c.Error(err)
				return nil
			}
			return nil
		})

		return upstreamHandler
	}
}

// Re-export types and functions from upstream
type Option = upstreamotelecho.Option

var (
	WithPropagators    = upstreamotelecho.WithPropagators
	WithTracerProvider = upstreamotelecho.WithTracerProvider
	WithSkipper        = upstreamotelecho.WithSkipper
)
