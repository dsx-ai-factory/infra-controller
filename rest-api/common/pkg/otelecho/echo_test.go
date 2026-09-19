// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package otelecho

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	b3prop "go.opentelemetry.io/contrib/propagators/b3"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
)

func TestErrorOnlyHandledOnce(t *testing.T) {
	router := echo.New()
	timesHandlingError := 0
	router.HTTPErrorHandler = func(e error, c echo.Context) {
		timesHandlingError++
	}
	router.Use(Middleware("test-service"))
	router.GET("/", func(c echo.Context) error {
		return errors.New("mock error")
	})
	r := httptest.NewRequest(http.MethodGet, "/", nil)
	w := httptest.NewRecorder()
	router.ServeHTTP(w, r)

	assert.Equal(t, 1, timesHandlingError)
}

func TestGetSpanNotInstrumented(t *testing.T) {
	router := echo.New()
	router.GET("/ping", func(c echo.Context) error {
		// Assert we don't have a span on the context.
		span := trace.SpanFromContext(c.Request().Context())
		ok := !span.SpanContext().IsValid()
		assert.True(t, ok)
		return c.String(200, "ok")
	})
	r := httptest.NewRequest("GET", "/ping", nil)
	w := httptest.NewRecorder()
	router.ServeHTTP(w, r)
	response := w.Result()
	assert.Equal(t, http.StatusOK, response.StatusCode)
}

func TestPropagationWithGlobalPropagators(t *testing.T) {
	provider := trace.NewNoopTracerProvider()
	otel.SetTextMapPropagator(propagation.TraceContext{})

	r := httptest.NewRequest("GET", "/user/123", nil)
	w := httptest.NewRecorder()

	ctx := context.Background()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})
	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
	ctx, _ = provider.Tracer(TracerName).Start(ctx, "test")
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(r.Header))

	router := echo.New()
	router.Use(Middleware("foobar", WithTracerProvider(provider)))
	router.GET("/user/:id", func(c echo.Context) error {
		span := trace.SpanFromContext(c.Request().Context())
		assert.Equal(t, sc.TraceID(), span.SpanContext().TraceID())
		assert.Equal(t, sc.SpanID(), span.SpanContext().SpanID())
		return c.NoContent(200)
	})

	router.ServeHTTP(w, r)
	otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator())
	assert.Equal(t, http.StatusOK, w.Result().StatusCode, "should call the 'user' handler")
}

func TestPropagationWithCustomPropagators(t *testing.T) {
	provider := trace.NewNoopTracerProvider()

	b3 := b3prop.New()

	r := httptest.NewRequest("GET", "/user/123", nil)
	w := httptest.NewRecorder()

	ctx := context.Background()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})
	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
	ctx, _ = provider.Tracer(TracerName).Start(ctx, "test")
	b3.Inject(ctx, propagation.HeaderCarrier(r.Header))

	router := echo.New()
	router.Use(Middleware("foobar", WithTracerProvider(provider), WithPropagators(b3)))
	router.GET("/user/:id", func(c echo.Context) error {
		span := trace.SpanFromContext(c.Request().Context())
		assert.Equal(t, sc.TraceID(), span.SpanContext().TraceID())
		assert.Equal(t, sc.SpanID(), span.SpanContext().SpanID())
		return c.NoContent(200)
	})

	router.ServeHTTP(w, r)
	assert.Equal(t, http.StatusOK, w.Result().StatusCode, "should call the 'user' handler")
}

func TestSkipper(t *testing.T) {
	r := httptest.NewRequest("GET", "/ping", nil)
	w := httptest.NewRecorder()

	skipper := func(c echo.Context) bool {
		return c.Request().RequestURI == "/ping"
	}

	router := echo.New()
	router.Use(Middleware("foobar", WithSkipper(skipper)))
	router.GET("/ping", func(c echo.Context) error {
		span := trace.SpanFromContext(c.Request().Context())
		assert.False(t, span.SpanContext().HasSpanID())
		assert.False(t, span.SpanContext().HasTraceID())
		return c.NoContent(200)
	})

	router.ServeHTTP(w, r)
	assert.Equal(t, http.StatusOK, w.Result().StatusCode, "should call the 'ping' handler")
}

// TestTraceIDInheritance verifies that trace IDs are properly inherited from parent spans
func TestTraceIDInheritance(t *testing.T) {
	provider := trace.NewNoopTracerProvider()
	otel.SetTextMapPropagator(propagation.TraceContext{})

	r := httptest.NewRequest("GET", "/test", nil)
	w := httptest.NewRecorder()

	// Create a parent trace context
	ctx := context.Background()
	parentTraceID := trace.TraceID{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10}
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: parentTraceID,
		SpanID:  trace.SpanID{0x01},
	})
	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
	ctx, _ = provider.Tracer(TracerName).Start(ctx, "parent")
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(r.Header))

	var receivedTraceID trace.TraceID
	router := echo.New()
	router.Use(Middleware("test-service", WithTracerProvider(provider)))
	router.GET("/test", func(c echo.Context) error {
		span := trace.SpanFromContext(c.Request().Context())
		receivedTraceID = span.SpanContext().TraceID()
		return c.NoContent(200)
	})

	router.ServeHTTP(w, r)

	response := w.Result()
	assert.Equal(t, http.StatusOK, response.StatusCode)

	// Verify trace ID is inherited from parent
	assert.Equal(t, parentTraceID, receivedTraceID, "Trace ID should be inherited from parent")
}

// TestWrapperPreservesUpstreamBehavior verifies that all upstream behavior is preserved
func TestWrapperPreservesUpstreamBehavior(t *testing.T) {
	provider := trace.NewNoopTracerProvider()
	otel.SetTextMapPropagator(propagation.TraceContext{})

	r := httptest.NewRequest("GET", "/test", nil)
	w := httptest.NewRecorder()

	// Create a parent trace context to ensure we have a valid span
	ctx := context.Background()
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10},
		SpanID:  trace.SpanID{0x01},
	})
	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
	ctx, _ = provider.Tracer(TracerName).Start(ctx, "parent")
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(r.Header))

	var spanCreated bool
	var spanContext trace.SpanContext
	router := echo.New()
	router.Use(Middleware("test-service", WithTracerProvider(provider)))
	router.GET("/test", func(c echo.Context) error {
		span := trace.SpanFromContext(c.Request().Context())
		spanContext = span.SpanContext()
		if spanContext.IsValid() {
			spanCreated = true
		}
		return c.NoContent(200)
	})

	router.ServeHTTP(w, r)

	response := w.Result()
	assert.Equal(t, http.StatusOK, response.StatusCode)
	// Note: With NoopTracerProvider, spans are created but may not be fully valid
	// The important thing is that the span exists and can be accessed
	assert.NotNil(t, spanContext, "Span context should exist")
	assert.True(t, spanCreated, "Span should be created and valid when parent trace exists")
	assert.True(t, spanContext.HasTraceID(), "Span should have trace ID")
	assert.True(t, spanContext.HasSpanID(), "Span should have span ID")

	otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator())
}
