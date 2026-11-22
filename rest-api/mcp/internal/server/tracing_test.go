// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json"
	"errors"
	"io"
	"maps"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/signal"
	"slices"
	"strconv"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/sirupsen/logrus"
	logtest "github.com/sirupsen/logrus/hooks/test"
	"github.com/stretchr/testify/require"
	urfave "github.com/urfave/cli/v2"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/propagation"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
	"go.opentelemetry.io/otel/trace/noop"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// resetTracing leaves no tracer provider or propagator behind for later tests.
// It sets empty ones instead of restoring the previous values, because the
// default global ones keep delegating to the first provider and propagator a
// test installs.
func resetTracing(t *testing.T) {
	t.Helper()
	t.Cleanup(func() {
		otel.SetTracerProvider(noop.NewTracerProvider())
		otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator())
	})
}

// TestTraceToolCalls protects the span one tool call records and the trace
// context its NICo REST request carries, driven through the real MCP handler
// so the middleware wiring and the REST client are part of the proof.
func TestTraceToolCalls(t *testing.T) {
	const (
		tool           = "nico_get_all_foo"
		inboundTraceID = "0123456789abcdef0123456789abcdef"
		inboundSpanID  = "0123456789abcdef"
	)
	// restContext is the trace context NICo REST receives.
	type restContext int
	const (
		noContext restContext = iota
		// spanContext is the recorded span's own context.
		spanContext
		// callerContext is the caller's context, unchanged.
		callerContext
	)
	for _, tc := range []struct {
		name string
		// exporting installs a recording tracer provider, as a configured OTLP
		// endpoint does. Otherwise spans are not recorded.
		exporting bool
		// propagators is OTEL_PROPAGATORS; empty keeps the W3C default.
		propagators string
		// callBaseURL starts the server with no base URL, so the call names
		// NICo REST in its base_url argument and brings a token of its own.
		callBaseURL bool
		// inbound sends the caller's trace context on the MCP request.
		inbound bool
		// upstreamStatus is the NICo REST response; 0 means 200.
		upstreamStatus int
		// method is the MCP method sent; empty sends tools/call.
		method string
		// tool is the called tool name; empty calls nico_get_all_foo.
		tool      string
		arguments map[string]any
		// wantSpan expects exactly one recorded span; otherwise none.
		wantSpan bool
		// wantErrorType is the span's error.type; empty means the span did not fail.
		wantErrorType    string
		wantRESTRequests int
		wantRESTContext  restContext
		// wantTools is the number of tools a tools/list response names.
		wantTools int
	}{
		{
			// The token argument and the inbound bearer must not reach the span.
			name:             "success records one server span that is the parent of the REST request",
			exporting:        true,
			arguments:        map[string]any{"token": "argument-token", "pageSize": 7},
			wantSpan:         true,
			wantRESTRequests: 1,
			wantRESTContext:  spanContext,
		},
		{
			// The REST error text can repeat caller input, so only its class is recorded.
			name:             "REST error fails the span as a tool error",
			exporting:        true,
			upstreamStatus:   http.StatusNotFound,
			wantSpan:         true,
			wantErrorType:    "tool_error",
			wantRESTRequests: 1,
			wantRESTContext:  spanContext,
		},
		{
			// The SDK rejects this before the tool handler runs, inside an HTTP 200 response.
			name:          "arguments that fail the input schema fail the span with the JSON-RPC code and send no REST request",
			exporting:     true,
			arguments:     map[string]any{"pageSize": "test"},
			wantSpan:      true,
			wantErrorType: "-32602",
		},
		{
			// A caller-supplied name must never become a span name or attribute.
			name:      "unknown tool records no span",
			exporting: true,
			tool:      "nico_not_a_tool",
		},
		{
			// Every MCP request passes through the middleware, not only tool calls.
			name:      "a request that is not a tool call is answered and records no span",
			exporting: true,
			method:    "tools/list",
			wantTools: 3,
		},
		{
			name:             "inbound trace context becomes the span's parent",
			exporting:        true,
			inbound:          true,
			wantSpan:         true,
			wantRESTRequests: 1,
			wantRESTContext:  spanContext,
		},
		{
			// A fixed W3C propagator would ignore the inbound header and send traceparent.
			name:             "OTEL_PROPAGATORS selects the format read and sent",
			exporting:        true,
			propagators:      "b3",
			inbound:          true,
			wantSpan:         true,
			wantRESTRequests: 1,
			wantRESTContext:  spanContext,
		},
		{
			// Like an inherited credential, the caller's tracestate and baggage
			// must not reach a destination the caller selects.
			name:             "a per-call base_url receives no trace context",
			exporting:        true,
			callBaseURL:      true,
			inbound:          true,
			wantSpan:         true,
			wantRESTRequests: 1,
		},
		{
			// With no OTEL_* variable set, the REST request is what it was before tracing.
			name:             "without export and inbound trace context the REST request carries none",
			wantRESTRequests: 1,
		},
		{
			// A hop that does not export must leave a gap in the trace, not split it.
			name:             "without export the inbound trace context is forwarded unchanged",
			inbound:          true,
			wantRESTRequests: 1,
			wantRESTContext:  callerContext,
		},
		{
			// A recording provider is installed here to prove the middleware is absent.
			name:             "OTEL_PROPAGATORS=none without export traces and forwards nothing",
			exporting:        true,
			propagators:      "none",
			inbound:          true,
			wantRESTRequests: 1,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			resetTracing(t)
			exporter := tracetest.NewInMemoryExporter()
			var provider trace.TracerProvider = noop.NewTracerProvider()
			if tc.exporting {
				// The sampler is fixed so an ambient OTEL_TRACES_SAMPLER cannot drop the span.
				provider = sdktrace.NewTracerProvider(sdktrace.WithSyncer(exporter), sdktrace.WithSampler(sdktrace.AlwaysSample()))
			}
			otel.SetTracerProvider(provider)
			// Export stays disabled so the provider above remains global; the
			// bootstrap still installs the propagator and decides whether tool
			// calls are traced.
			t.Setenv("OTEL_PROPAGATORS", tc.propagators)
			shutdown, err := cotel.Bootstrap(context.Background(), false, "")
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, shutdown(context.Background())) })

			// contextHeaders are the headers that carry a trace context in the
			// row's format. The caller's tracestate and baggage travel with a
			// W3C context the caller started.
			contextHeaders := func(traceID, spanID string) map[string]string {
				if tc.propagators == "b3" {
					return map[string]string{"B3": traceID + "-" + spanID + "-1"}
				}
				headers := map[string]string{"Traceparent": "00-" + traceID + "-" + spanID + "-01"}
				if tc.inbound {
					headers["Tracestate"] = "vendor=opaque"
					headers["Baggage"] = "tenant=acme"
				}
				return headers
			}

			var mu sync.Mutex
			var restHeaders []http.Header
			upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				mu.Lock()
				restHeaders = append(restHeaders, r.Header.Clone())
				mu.Unlock()
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(cmp.Or(tc.upstreamStatus, http.StatusOK))
				_, _ = w.Write([]byte(`{}`))
			}))
			defer upstream.Close()
			metrics := NewMetrics()
			opts := Options{BaseURL: upstream.URL, Org: "tester", Metrics: metrics}
			arguments := map[string]any{}
			maps.Copy(arguments, tc.arguments)
			if tc.callBaseURL {
				opts.BaseURL = ""
				arguments["base_url"] = upstream.URL
				arguments["token"] = "argument-token"
			}
			server, err := BuildServer([]byte(synthSpec), opts)
			require.NoError(t, err)
			ts := httptest.NewServer(NewHandler(server))
			defer ts.Close()

			params := map[string]any{"name": cmp.Or(tc.tool, tool), "arguments": arguments}
			if tc.method != "" {
				params = map[string]any{}
			}
			req, err := http.NewRequest(http.MethodPost, ts.URL, bytes.NewReader(jsonrpcRequest(1, cmp.Or(tc.method, "tools/call"), params)))
			require.NoError(t, err)
			req.Header.Set("Content-Type", "application/json")
			req.Header.Set("Accept", "application/json, text/event-stream")
			req.Header.Set("Authorization", "Bearer inbound-bearer")
			if tc.inbound {
				for name, value := range contextHeaders(inboundTraceID, inboundSpanID) {
					req.Header.Set(name, value)
				}
			}
			resp, err := http.DefaultClient.Do(req)
			require.NoError(t, err)
			defer resp.Body.Close()
			body, err := io.ReadAll(resp.Body)
			require.NoError(t, err)
			require.Equal(t, http.StatusOK, resp.StatusCode)
			require.Len(t, decodeToolList(t, body), tc.wantTools, "tools listed")

			mu.Lock()
			defer mu.Unlock()
			require.Len(t, restHeaders, tc.wantRESTRequests, "NICo REST requests")
			gotRESTContext := map[string]string{}
			for _, headers := range restHeaders {
				for _, name := range []string{"Traceparent", "Tracestate", "Baggage", "B3"} {
					value := headers.Get(name)
					if value != "" {
						gotRESTContext[name] = value
					}
				}
			}
			wantRESTContext := map[string]string{}
			spans := exporter.GetSpans()
			if !tc.wantSpan {
				require.Empty(t, spans)
				if tc.wantRESTContext == callerContext {
					wantRESTContext = contextHeaders(inboundTraceID, inboundSpanID)
				}
				require.Equal(t, wantRESTContext, gotRESTContext)
				return
			}
			require.Len(t, spans, 1)
			span := spans[0]
			require.Equal(t, "tools/call "+tool, span.Name)
			require.Equal(t, trace.SpanKindServer, span.SpanKind)
			// The exact attribute set, the empty status description, and the
			// absence of events together prove no argument, token, or error
			// text is recorded.
			wantAttributes := []attribute.KeyValue{
				attribute.String("mcp.method.name", "tools/call"),
				attribute.String("gen_ai.operation.name", "execute_tool"),
				attribute.String("gen_ai.tool.name", tool),
			}
			wantStatus := sdktrace.Status{Code: codes.Unset}
			// A span fails exactly when the tool call metric counts an error.
			wantResult := "success"
			if tc.wantErrorType != "" {
				wantAttributes = append(wantAttributes, attribute.String("error.type", tc.wantErrorType))
				wantStatus = sdktrace.Status{Code: codes.Error}
				wantResult = "error"
			}
			require.ElementsMatch(t, wantAttributes, span.Attributes)
			require.Equal(t, wantStatus, span.Status)
			require.Empty(t, span.Events)
			require.Equal(t, 1.0, testutil.ToFloat64(metrics.toolCalls.WithLabelValues(tool, wantResult)))
			// The caller still gets the tool's own answer: its result, or the
			// JSON-RPC error of a call the SDK rejects.
			var answer struct {
				Result *rpcToolCallResult `json:"result"`
				Error  *struct {
					Code int64 `json:"code"`
				} `json:"error"`
			}
			require.NoError(t, json.Unmarshal(body, &answer))
			switch tc.wantErrorType {
			case "", "tool_error":
				require.NotNil(t, answer.Result, "the answer has no result: %s", body)
				require.Equal(t, tc.wantErrorType != "", answer.Result.IsError)
				require.NotEmpty(t, firstText(*answer.Result))
			default:
				require.NotNil(t, answer.Error, "the answer has no JSON-RPC error: %s", body)
				require.Equal(t, tc.wantErrorType, strconv.FormatInt(answer.Error.Code, 10))
			}

			if tc.inbound {
				require.Equal(t, inboundTraceID, span.SpanContext.TraceID().String())
				require.Equal(t, inboundSpanID, span.Parent.SpanID().String())
				require.True(t, span.Parent.IsRemote())
			} else {
				require.False(t, span.Parent.IsValid(), "a call with no inbound trace context starts a trace")
			}
			if tc.wantRESTContext == spanContext {
				wantRESTContext = contextHeaders(span.SpanContext.TraceID().String(), span.SpanContext.SpanID().String())
			}
			require.Equal(t, wantRESTContext, gotRESTContext)
		})
	}
}

// TestToolCallErrorType protects the fallback that keeps error.type bounded
// for failures the MCP handler cannot produce in a test.
func TestToolCallErrorType(t *testing.T) {
	for _, tc := range []struct {
		name   string
		result mcp.Result
		err    error
		want   string
	}{
		// Error text never becomes an attribute value.
		{name: "error without a JSON-RPC code", err: errors.New("dial tcp 10.0.0.1:8388: connection refused"), want: "_OTHER"},
		// The tool call metric counts a missing result as an error, so the span must too.
		{name: "no result and no error", want: "_OTHER"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			require.Equal(t, tc.want, toolCallErrorType(tc.result, tc.err))
		})
	}
}

// TestRun protects the tracing the real binary starts: an endpoint variable
// turns export on before the MCP server is built, a bad tracing variable is
// logged and does not stop the server, and the spans still queued are flushed
// when it stops, after the listeners have drained and within the flush timeout.
func TestRun(t *testing.T) {
	for _, tc := range []struct {
		name string
		// scheduleDelay is OTEL_BSP_SCHEDULE_DELAY, in milliseconds.
		scheduleDelay string
		// inFlight sends SIGTERM while the tool call is waiting for NICo REST.
		inFlight bool
		// collectorHangs makes the collector take the export and never answer.
		collectorHangs bool
		// wantExports is the number of OTLP requests the collector receives,
		// none of them before the server stops.
		wantExports int
		// wantErrorLogs are the error-level messages Run logs.
		wantErrorLogs []string
	}{
		{
			// The batch delay outlasts the test, so only the flush on shutdown can deliver the span.
			name:          "an endpoint variable exports the tool call span when the server stops",
			scheduleDelay: "10000",
			wantExports:   1,
		},
		{
			name:          "a bad tracing variable is logged and leaves the server running without tracing",
			scheduleDelay: "not-a-number",
			wantErrorLogs: []string{"nico-mcp: failed to initialize tracing: OTEL_BSP_SCHEDULE_DELAY must be an integer from 100 through 10000"},
		},
		{
			// The call's span ends while the listeners drain, so a flush before the drain would lose it.
			name:          "the span of a call in flight at SIGTERM is exported",
			scheduleDelay: "10000",
			inFlight:      true,
			wantExports:   1,
		},
		{
			// Only the flush timeout ends this wait; without it the exporter retries for 30 seconds.
			name:           "a collector that does not answer holds shutdown for the flush timeout only",
			scheduleDelay:  "10000",
			collectorHangs: true,
			wantExports:    1,
			wantErrorLogs:  []string{"nico-mcp: failed to shut down tracing: context deadline exceeded"},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			resetTracing(t)
			// Run logs through the standard logrus logger.
			logs := &logtest.Hook{}
			previousHooks := logrus.StandardLogger().ReplaceHooks(logrus.LevelHooks{})
			logrus.StandardLogger().AddHook(logs)
			t.Cleanup(func() { logrus.StandardLogger().ReplaceHooks(previousHooks) })

			// release lets a held collector or NICo REST request finish.
			release := make(chan struct{})
			var releaseOnce sync.Once
			releaseHeld := func() { releaseOnce.Do(func() { close(release) }) }

			var mu sync.Mutex
			var exports [][]byte
			exported := func() [][]byte {
				mu.Lock()
				defer mu.Unlock()
				return slices.Clone(exports)
			}
			collector := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
				body, _ := io.ReadAll(r.Body)
				mu.Lock()
				exports = append(exports, body)
				mu.Unlock()
				if tc.collectorHangs {
					<-release
				}
			}))
			defer collector.Close()
			arrived := make(chan struct{}, 1)
			upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				if tc.inFlight {
					arrived <- struct{}{}
					<-release
				}
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`{}`))
			}))
			defer upstream.Close()
			// Deferred last so it runs first: the servers wait for held requests to finish.
			defer releaseHeld()

			// Only the two variables set last configure tracing here, whatever
			// the environment the tests run in.
			for _, name := range []string{
				"OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "OTEL_EXPORTER_OTLP_PROTOCOL", "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
				"OTEL_EXPORTER_OTLP_COMPRESSION", "OTEL_EXPORTER_OTLP_TRACES_COMPRESSION", "OTEL_SERVICE_NAME",
				"OTEL_RESOURCE_ATTRIBUTES", "OTEL_TRACES_SAMPLER", "OTEL_PROPAGATORS",
				"OTEL_EXPORTER_OTLP_TIMEOUT", "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", "OTEL_BSP_EXPORT_TIMEOUT",
			} {
				t.Setenv(name, "")
			}
			t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", collector.URL)
			t.Setenv("OTEL_BSP_SCHEDULE_DELAY", tc.scheduleDelay)

			listener, err := net.Listen("tcp", "127.0.0.1:0")
			require.NoError(t, err)
			address := listener.Addr().String()
			require.NoError(t, listener.Close())

			// The test takes SIGTERM as well, so one sent before Run listens for
			// it cannot end the test binary.
			signals := make(chan os.Signal, 1)
			signal.Notify(signals, syscall.SIGTERM)
			defer signal.Stop(signals)

			app := &urfave.App{
				Flags: ServeFlags(),
				Action: func(c *urfave.Context) error {
					return Run(c, []byte(synthSpec))
				},
			}
			done := make(chan error, 1)
			go func() {
				done <- app.Run([]string{"nico-mcp", "--listen", address, "--base-url", upstream.URL, "--org", "tester"})
			}()
			listening := func() bool {
				conn, dialErr := net.Dial("tcp", address)
				if dialErr != nil {
					return false
				}
				_ = conn.Close()
				return true
			}
			require.Eventually(t, listening, 10*time.Second, 10*time.Millisecond, "nico-mcp is not listening")

			// The call runs beside the test, so SIGTERM can arrive while it waits.
			type callResult struct {
				status int
				body   []byte
				err    error
			}
			called := make(chan callResult, 1)
			go func() {
				var result callResult
				defer func() { called <- result }()
				var req *http.Request
				req, result.err = http.NewRequest(http.MethodPost, "http://"+address+"/mcp", bytes.NewReader(jsonrpcRequest(1, "tools/call", map[string]any{
					"name":      "nico_get_all_foo",
					"arguments": map[string]any{},
				})))
				if result.err != nil {
					return
				}
				req.Header.Set("Content-Type", "application/json")
				req.Header.Set("Accept", "application/json, text/event-stream")
				var resp *http.Response
				resp, result.err = http.DefaultClient.Do(req)
				if result.err != nil {
					return
				}
				defer resp.Body.Close()
				result.status = resp.StatusCode
				result.body, result.err = io.ReadAll(resp.Body)
			}()
			requireCalled := func() {
				result := <-called
				require.NoError(t, result.err)
				require.Equal(t, http.StatusOK, result.status)
				require.False(t, decodeToolCallResult(t, result.body).IsError, "tool call failed: %s", result.body)
			}
			if tc.inFlight {
				select {
				case <-arrived:
				case result := <-called:
					require.Failf(t, "the tool call ended without waiting for NICo REST", "status %d, error %v: %s", result.status, result.err, result.body)
				}
			} else {
				requireCalled()
			}
			require.Empty(t, exported(), "exports before the server stops")

			// The signal is sent again when Run was not yet listening for it.
			// The test sees each one arrive before going on, so none is still
			// on its way when the test stops taking them.
			process, err := os.FindProcess(os.Getpid())
			require.NoError(t, err)
			sigterm := func() {
				_ = process.Signal(syscall.SIGTERM)
				<-signals
			}
			if tc.inFlight {
				// A closed listener shows that Run is draining the call.
				require.Eventually(t, func() bool {
					sigterm()
					return !listening()
				}, 10*time.Second, 10*time.Millisecond, "the listener did not close after SIGTERM")
				require.Empty(t, exported(), "exports while the call is in flight")
				releaseHeld()
				requireCalled()
			}
			if tc.collectorHangs {
				// Uptime before the stop, so a flush timeout that began when
				// the server started would run out early.
				time.Sleep(250 * time.Millisecond)
			}
			stopping := time.Now()
			var runErr error
			require.Eventually(t, func() bool {
				sigterm()
				select {
				case runErr = <-done:
					return true
				case <-time.After(50 * time.Millisecond):
					return false
				}
			}, tracingShutdownTimeout+3*time.Second, time.Millisecond, "Run did not return within the flush timeout after SIGTERM")
			require.NoError(t, runErr)
			if tc.collectorHangs {
				// The flush gets its whole timeout, counted from the stop.
				require.GreaterOrEqual(t, time.Since(stopping), 5*time.Second, "the flush gave up before its timeout")
			}

			flushed := exported()
			require.Len(t, flushed, tc.wantExports)
			for _, export := range flushed {
				require.True(t, bytes.Contains(export, []byte("nico-mcp")), "the export does not name the nico-mcp service")
				require.True(t, bytes.Contains(export, []byte("tools/call nico_get_all_foo")), "the export does not hold the tool call span")
			}
			var errorLogs []string
			for _, entry := range logs.AllEntries() {
				if entry.Level == logrus.ErrorLevel {
					errorLogs = append(errorLogs, entry.Message)
				}
			}
			require.Equal(t, tc.wantErrorLogs, errorLogs)
		})
	}
}
