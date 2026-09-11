// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package otelecho

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// TestFullIntegration verifies the complete flow with a real SDK provider: the
// middleware extracts the parent trace from headers, and child spans created via
// the shared global-provider helper parent to the server span.
func TestFullIntegration(t *testing.T) {
	recorder := tracetest.NewSpanRecorder()
	tp := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	prevTP := otel.GetTracerProvider()
	otel.SetTracerProvider(tp)
	defer otel.SetTracerProvider(prevTP)
	otel.SetTextMapPropagator(propagation.TraceContext{})
	defer otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator())

	r := httptest.NewRequest("GET", "/test", nil)
	w := httptest.NewRecorder()

	// Create a parent trace context
	ctx := context.Background()
	parentTraceID := trace.TraceID{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10}
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID:    parentTraceID,
		SpanID:     trace.SpanID{0x01},
		TraceFlags: trace.FlagsSampled,
	})
	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(r.Header))

	var serverSpanID trace.SpanID

	router := echo.New()
	router.Use(Middleware("test-service", WithTracerProvider(tp)))
	router.GET("/test", func(c echo.Context) error {
		reqCtx := c.Request().Context()
		serverSpanID = trace.SpanFromContext(reqCtx).SpanContext().SpanID()

		// Simulate application code creating a child operation span.
		_, childSpan := cotel.StartSpan(reqCtx, "child-span")
		childSpan.End()

		return c.NoContent(200)
	})

	router.ServeHTTP(w, r)

	response := w.Result()
	assert.Equal(t, http.StatusOK, response.StatusCode)

	// The recorded child span must parent to the server span in the same trace
	var childFound bool
	for _, span := range recorder.Ended() {
		if span.Name() == "child-span" {
			childFound = true
			assert.Equal(t, parentTraceID, span.SpanContext().TraceID(), "child joins the propagated trace")
			assert.Equal(t, serverSpanID, span.Parent().SpanID(), "child parents to the server span")
		}
	}
	assert.True(t, childFound, "child span should be recorded")
}

// TestOriginalBehaviorMatch verifies that the wrapper behaves exactly like the original
// for all the original test cases
func TestOriginalBehaviorMatch(t *testing.T) {
	provider := trace.NewNoopTracerProvider()
	otel.SetTextMapPropagator(propagation.TraceContext{})

	tests := []struct {
		name            string
		setupRequest    func() *http.Request
		expectedTraceID string
	}{
		{
			name: "with parent trace",
			setupRequest: func() *http.Request {
				r := httptest.NewRequest("GET", "/test", nil)
				ctx := context.Background()
				sc := trace.NewSpanContext(trace.SpanContextConfig{
					TraceID: trace.TraceID{0xAA, 0xBB, 0xCC, 0xDD},
					SpanID:  trace.SpanID{0x01},
				})
				ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
				ctx, _ = provider.Tracer(TracerName).Start(ctx, "parent")
				otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(r.Header))
				return r
			},
			expectedTraceID: "aabbccdd000000000000000000000000",
		},
		{
			name: "without parent trace",
			setupRequest: func() *http.Request {
				return httptest.NewRequest("GET", "/test", nil)
			},
			expectedTraceID: "00000000000000000000000000000000", // Empty trace ID when no parent
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := tt.setupRequest()
			w := httptest.NewRecorder()

			var receivedTraceID string
			router := echo.New()
			router.Use(Middleware("test-service", WithTracerProvider(provider)))
			router.GET("/test", func(c echo.Context) error {
				span := trace.SpanFromContext(c.Request().Context())
				receivedTraceID = span.SpanContext().TraceID().String()
				return c.NoContent(200)
			})

			router.ServeHTTP(w, r)
			assert.Equal(t, http.StatusOK, w.Result().StatusCode)
			assert.Equal(t, tt.expectedTraceID, receivedTraceID, "Trace ID should match expected")
		})
	}

	otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator())
}
