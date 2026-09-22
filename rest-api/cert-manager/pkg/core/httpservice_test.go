// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"context"
	"fmt"
	"io/ioutil"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/mux"
	"github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"go.opentelemetry.io/otel"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/trace"
)

func TestHTTPServiceRoutes(t *testing.T) {
	ctx := WithDefaultLogger(context.Background())

	s := NewHTTPService("")
	s.AddHealthRoute(ctx)
	s.AddVersionRoute(ctx)
	s.AddMetricsRoute(ctx)
	s.AddAdminRoute(ctx)
	s.Use(NewHTTPMiddleware(ctx, WithRequestMetrics("testroutes"),
		WithHandlerTimeout(500*time.Millisecond),
		WithRequestBodyLimit(int64(10)))...)

	run := func(method, path, payload string) (string, int) {
		req := httptest.NewRequest(method, path, strings.NewReader(payload))
		w := httptest.NewRecorder()
		s.ServeHTTP(w, req)

		resp := w.Result()
		body, _ := ioutil.ReadAll(resp.Body)
		return string(body), resp.StatusCode
	}

	body, code := run("GET", "/notexist", "")
	assert.Contains(t, body, "not found")
	assert.Equal(t, http.StatusNotFound, code)

	body, code = run("GET", "/healthz", "")
	assert.Equal(t, "ok\n", body)
	assert.Equal(t, http.StatusOK, code)

	body, code = run("GET", "/version", "")
	assert.Equal(t, "0.1.0\n", body)
	assert.Equal(t, http.StatusOK, code)

	body, code = run("GET", "/metrics", "")
	assert.Contains(t, body, "testroutes_http_duration_seconds")
	assert.Equal(t, http.StatusOK, code)

	assert.Equal(t, logrus.InfoLevel, GetLogger(ctx).Logger.GetLevel())
	body, code = run("GET", "/admin", "")
	assert.Contains(t, body, `"log-level": "info"`)
	assert.Equal(t, http.StatusOK, code)

	body, code = run("GET", "/admin?log-level=notexist", "")
	assert.Contains(t, body, "not a valid logrus Level")
	assert.Equal(t, http.StatusBadRequest, code)

	body, code = run("GET", "/admin?log-level=debug", "")
	assert.Contains(t, body, `"log-level": "debug"`)
	assert.Equal(t, http.StatusOK, code)

	body, code = run("GET", "/admin", "")
	assert.Contains(t, body, `"log-level": "debug"`)
	assert.Equal(t, http.StatusOK, code)
	assert.Equal(t, logrus.DebugLevel, GetLogger(ctx).Logger.GetLevel())

	// test request body limit
	{
		s.HandleFunc("/echo", func(w http.ResponseWriter, r *http.Request) {
			body, err := ioutil.ReadAll(r.Body)
			if err != nil {
				http.Error(w, err.Error(), http.StatusBadRequest)
				return
			}
			w.WriteHeader(http.StatusOK)
			w.Write(body)
		}).Methods("POST")

		body, code := run("POST", "/echo", "123")
		assert.Equal(t, "123", body)
		assert.Equal(t, http.StatusOK, code)

		body, code = run("POST", "/echo", "123456789abcdef")
		assert.Contains(t, body, "request body too large")
		assert.Equal(t, http.StatusBadRequest, code)
	}

	// test request handler timeout, handlerTimeout was set to 500ms.
	{
		makeSlowAPIHandler := func(d time.Duration) func(w http.ResponseWriter, r *http.Request) {
			return func(w http.ResponseWriter, r *http.Request) {
				log := GetLogger(r.Context())

				select {
				case <-r.Context().Done():
					log.Infof("SlowAPICall was supposed to take %v, but was canceled. Err: %v", d, r.Context().Err())
				case <-time.After(d):
					log.Printf("SlowAPIcall done after %v", d)
					w.WriteHeader(http.StatusOK)
					w.Write([]byte("ok\n"))
				}
			}
		}
		s.HandleFunc("/100ms", makeSlowAPIHandler(100*time.Millisecond)).Methods("GET")
		s.HandleFunc("/30s", makeSlowAPIHandler(30*time.Second)).Methods("GET")

		body, code := run("GET", "/100ms", "")
		assert.Equal(t, "ok\n", body)
		assert.Equal(t, http.StatusOK, code)

		body, code = run("GET", "/30s", "")
		assert.Contains(t, body, "Request timed out")
		assert.Equal(t, http.StatusServiceUnavailable, code)
	}
}

func TestHTTPServiceStart(t *testing.T) {
	ctx := WithDefaultLogger(context.Background())

	get := func(c *http.Client, url string) (string, int) {
		resp, _ := c.Get(url)
		body, _ := ioutil.ReadAll(resp.Body)
		return string(body), resp.StatusCode
	}

	{
		ctx, cancel := context.WithCancel(ctx)

		socketPath := "/tmp/httpserver-test.sock"
		defer os.Remove(socketPath)

		s := NewHTTPService("unix://" + socketPath)
		s.AddHealthRoute(ctx)
		s.Use(NewHTTPMiddleware(ctx)...)
		s.Start(ctx)

		c := &http.Client{
			Timeout: 10 * time.Second,
			Transport: &http.Transport{
				DialContext: func(_ context.Context, _, _ string) (net.Conn, error) {
					return net.Dial("unix", socketPath)
				},
			},
		}
		body, code := get(c, "http://localhost/healthz")
		assert.Equal(t, "ok\n", body)
		assert.Equal(t, http.StatusOK, code)

		cancel()
	}

	{
		ctx, cancel := context.WithCancel(ctx)
		s := NewHTTPService("0.0.0.0:0")
		s.AddHealthRoute(ctx)
		s.Use(NewHTTPMiddleware(ctx)...)
		ln, err := s.Start(ctx)
		assert.Nil(t, err)

		t.Logf("addr: %s", ln.Addr().(*net.TCPAddr).String())
		addr := ln.Addr().(*net.TCPAddr).String()

		c := &http.Client{Timeout: 10 * time.Second}
		body, code := get(c, fmt.Sprintf("http://%s/healthz", addr))
		assert.Equal(t, "ok\n", body)
		assert.Equal(t, http.StatusOK, code)

		cancel()
	}
}

func TestWithRequestMetrics(t *testing.T) {
	tests := []struct {
		name       string
		serverName string
		envValue   string
		want       string
	}{
		{
			name:       "uses the supplied prefix when the environment is unset",
			serverName: "nico_rest_cert_manager",
			want:       "nico_rest_cert_manager",
		},
		{
			name:       "METRICS_NAMESPACE takes precedence",
			serverName: "nico_rest_cert_manager",
			envValue:   "acme_cert_manager",
			want:       "acme_cert_manager",
		},
		{
			// An empty value is indistinguishable from unset, and an empty prefix
			// would expose bare names like "http_duration_seconds".
			name:       "an empty METRICS_NAMESPACE leaves the supplied prefix alone",
			serverName: "nico_rest_cert_manager",
			envValue:   "",
			want:       "nico_rest_cert_manager",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv(MetricsNamespaceEnv, tt.envValue)

			h := &httpMiddleware{}
			WithRequestMetrics(tt.serverName)(h)

			assert.Equal(t, tt.want, h.latencyMetricsName)
		})
	}
}

func Test_telemetryMiddleware(t *testing.T) {
	otel.SetTracerProvider(sdktrace.NewTracerProvider())

	router := mux.NewRouter()
	router.Use(NewHTTPMiddleware(context.Background(), WithTelemetry("svc"))...)
	router.HandleFunc("/test-with-telemetry", func(_ http.ResponseWriter, r *http.Request) {
		// Pull the span from the context should be a noop
		tSpan := trace.SpanFromContext(r.Context())
		assert.True(t, tSpan.SpanContext().HasSpanID())
	})
	router.HandleFunc(httpHealthRoutePath, func(_ http.ResponseWriter, r *http.Request) {
		// Pull the span from the context should be a noop
		tSpan := trace.SpanFromContext(r.Context())
		assert.False(t, tSpan.SpanContext().HasSpanID())
	})

	r := httptest.NewRequest("GET", "/test-with-telemetry", nil)
	router.ServeHTTP(httptest.NewRecorder(), r)

	r = httptest.NewRequest("GET", httpHealthRoutePath, nil)
	router.ServeHTTP(httptest.NewRecorder(), r)
}

func Test_recordingResponseWriter(t *testing.T) {
	// make sure the recordingResponseWriter preserves interfaces implemented by the wrapped writer
	router := mux.NewRouter()

	// Ensure response is properly recorded
	router.HandleFunc("/test", http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusTeapot)
	}))
	r := httptest.NewRequest("GET", "/test", nil)
	w1 := getRRW(httptest.NewRecorder())
	defer putRRW(w1)

	router.ServeHTTP(w1.writer, r)
	assert.Equal(t, http.StatusTeapot, w1.statusCode)
}
