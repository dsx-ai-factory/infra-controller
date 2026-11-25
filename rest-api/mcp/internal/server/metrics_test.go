// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/require"
	urfave "github.com/urfave/cli/v2"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

// TestRegisterGET protects what one tool call adds to each metric, driven
// through the real MCP handler so the handler wiring is part of the proof.
func TestRegisterGET(t *testing.T) {
	for _, tc := range []struct {
		name string
		// upstreamStatus is the NICo REST response; 0 means REST must not be called.
		upstreamStatus int
		// configuredBaseURL pins the server to the upstream.
		configuredBaseURL bool
		authorization     string
		// tool is the called tool name; empty calls nico_get_all_foo.
		tool      string
		arguments func(upstreamURL string) map[string]any
		// wantResult is the tool call's result label; empty means the call is not counted.
		wantResult     string
		wantRESTStatus string
		wantRejection  string
	}{
		{
			name:              "success counts the tool call and its REST status",
			upstreamStatus:    http.StatusOK,
			configuredBaseURL: true,
			wantResult:        "success",
			wantRESTStatus:    "200",
		},
		{
			// The REST status is the only place an operator sees why the tool failed.
			name:              "REST error counts a failed tool call under its status",
			upstreamStatus:    http.StatusNotFound,
			configuredBaseURL: true,
			wantResult:        "error",
			wantRESTStatus:    "404",
		},
		{
			name:              "another base URL is a rejection and sends no REST request",
			configuredBaseURL: true,
			arguments: func(string) map[string]any {
				return map[string]any{"base_url": "https://other.example.com"}
			},
			wantResult:    "error",
			wantRejection: rejectionBaseURLMismatch,
		},
		{
			name:          "inherited bearer for a per-call base URL is a rejection and sends no REST request",
			authorization: "Bearer inbound-bearer",
			arguments: func(upstreamURL string) map[string]any {
				return map[string]any{"base_url": upstreamURL}
			},
			wantResult:    "error",
			wantRejection: rejectionInheritedCredentials,
		},
		{
			// A failure that is not about credentials must not look like a rejection.
			name:       "missing base URL fails without counting a rejection",
			wantResult: "error",
		},
		{
			// The SDK rejects this before the tool handler runs, inside an HTTP 200 response.
			name:              "arguments that fail the input schema count a failed tool call and send no REST request",
			configuredBaseURL: true,
			arguments: func(string) map[string]any {
				return map[string]any{"pageSize": "test"}
			},
			wantResult: "error",
		},
		{
			// A caller-supplied name must never become a tool label.
			name:              "unknown tool is not counted",
			configuredBaseURL: true,
			tool:              "nico_not_a_tool",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var upstreamRequests atomic.Int64
			upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				upstreamRequests.Add(1)
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(cmp.Or(tc.upstreamStatus, http.StatusOK))
				_, _ = w.Write([]byte(`{}`))
			}))
			defer upstream.Close()
			metrics := NewMetrics()
			opts := Options{Org: "tester", Metrics: metrics}
			if tc.configuredBaseURL {
				opts.BaseURL = upstream.URL
			}
			server, err := BuildServer([]byte(synthSpec), opts)
			require.NoError(t, err)
			ts := httptest.NewServer(NewHandler(server))
			defer ts.Close()
			arguments := map[string]any{}
			if tc.arguments != nil {
				arguments = tc.arguments(upstream.URL)
			}

			tool := cmp.Or(tc.tool, "nico_get_all_foo")
			resp := mcpPost(t, ts.URL, tc.authorization, jsonrpcRequest(1, "tools/call", map[string]any{
				"name":      tool,
				"arguments": arguments,
			}))
			defer resp.Body.Close()
			require.Equal(t, http.StatusOK, resp.StatusCode)

			if tc.wantResult == "" {
				require.Zero(t, testutil.CollectAndCount(metrics.toolCalls))
				require.Zero(t, testutil.CollectAndCount(metrics.toolDuration))
			} else {
				require.Equal(t, 1, testutil.CollectAndCount(metrics.toolCalls))
				require.Equal(t, 1.0, testutil.ToFloat64(metrics.toolCalls.WithLabelValues(tool, tc.wantResult)))
				require.Equal(t, 1, testutil.CollectAndCount(metrics.toolDuration))
			}
			if tc.wantRESTStatus == "" {
				require.Zero(t, upstreamRequests.Load(), "NICo REST received a request")
				require.Zero(t, testutil.CollectAndCount(metrics.restRequests))
				require.Zero(t, testutil.CollectAndCount(metrics.restDuration))
			} else {
				require.Equal(t, 1, testutil.CollectAndCount(metrics.restRequests))
				require.Equal(t, 1.0, testutil.ToFloat64(metrics.restRequests.WithLabelValues(tool, tc.wantRESTStatus)))
				require.Equal(t, 1, testutil.CollectAndCount(metrics.restDuration))
			}
			for _, reason := range []string{rejectionBaseURLMismatch, rejectionInheritedCredentials} {
				want := 0.0
				if reason == tc.wantRejection {
					want = 1
				}
				require.Equal(t, want, testutil.ToFloat64(metrics.credentialRejections.WithLabelValues(reason)), reason)
			}
		})
	}
}

// TestRestStatusLabel keeps the status_code label bounded when REST sends no response.
func TestRestStatusLabel(t *testing.T) {
	for _, tc := range []struct {
		name  string
		event appcli.RequestEvent
		want  string
	}{
		{name: "response status", event: appcli.RequestEvent{StatusCode: http.StatusServiceUnavailable}, want: "503"},
		// net/http wraps the context error, so the label must survive wrapping.
		{name: "deadline", event: appcli.RequestEvent{Err: fmt.Errorf("Get: %w", context.DeadlineExceeded)}, want: restStatusTimeout},
		{name: "cancellation", event: appcli.RequestEvent{Err: fmt.Errorf("Get: %w", context.Canceled)}, want: restStatusCanceled},
		// A dial timeout reaches the client as a net.Error, not as the context deadline.
		{name: "transport timeout", event: appcli.RequestEvent{Err: &url.Error{Op: "Get", URL: "http://nico-rest-api:8388",
			Err: &net.OpError{Op: "dial", Net: "tcp", Err: os.ErrDeadlineExceeded}}}, want: restStatusTimeout},
		// A response status wins over any error recorded with it.
		{name: "response status with a timeout error", event: appcli.RequestEvent{StatusCode: http.StatusGatewayTimeout,
			Err: os.ErrDeadlineExceeded}, want: "504"},
		// Error text never becomes a label value.
		{name: "other failure", event: appcli.RequestEvent{Err: errors.New("dial tcp 10.0.0.1:8388: connection refused")}, want: restStatusError},
	} {
		t.Run(tc.name, func(t *testing.T) {
			require.Equal(t, tc.want, restStatusLabel(tc.event))
		})
	}
}

// TestMetrics_observeRESTRequest protects the latency an operator compares against tool latency.
func TestMetrics_observeRESTRequest(t *testing.T) {
	for _, tc := range []struct {
		name     string
		duration time.Duration
		want     string
	}{
		// A REST read that uses the whole tool budget must not fall into +Inf only.
		{name: "full read budget lands in a finite bucket", duration: toolReadTimeout, want: `
# HELP nico_mcp_rest_request_duration_seconds Time from sending one NICo REST request to reading its full response body, by tool.
# TYPE nico_mcp_rest_request_duration_seconds histogram
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.001"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.0025"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.005"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.01"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.025"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.05"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.1"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.25"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="0.5"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="1"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="2.5"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="5"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="10"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="30"} 0
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="60"} 1
nico_mcp_rest_request_duration_seconds_bucket{tool="nico_get_foo",le="+Inf"} 1
nico_mcp_rest_request_duration_seconds_sum{tool="nico_get_foo"} 55
nico_mcp_rest_request_duration_seconds_count{tool="nico_get_foo"} 1
`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			metrics := NewMetrics()

			metrics.observeRESTRequest("nico_get_foo", appcli.RequestEvent{StatusCode: http.StatusOK, Duration: tc.duration})

			require.NoError(t, testutil.CollectAndCompare(metrics.restDuration, strings.NewReader(tc.want)))
		})
	}
}

// TestMetrics_instrumentHTTP protects the whole-request series for the MCP endpoint.
func TestMetrics_instrumentHTTP(t *testing.T) {
	for _, tc := range []struct {
		name   string
		method string
		status int
	}{
		{name: "served request", method: http.MethodPost, status: http.StatusOK},
		// Requests the MCP handler refuses never reach a tool, so only this series shows them.
		{name: "refused request", method: http.MethodGet, status: http.StatusMethodNotAllowed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			metrics := NewMetrics()
			handler := metrics.instrumentHTTP(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(tc.status)
			}))

			handler.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(tc.method, "/mcp", nil))

			method := strings.ToLower(tc.method)
			require.Equal(t, 1.0, testutil.ToFloat64(metrics.httpRequests.WithLabelValues(method, strconv.Itoa(tc.status))))
			require.Equal(t, 1, testutil.CollectAndCount(metrics.httpDuration))
		})
	}
}

// TestNewMetricsServer protects what the metrics listener exposes and what it does not.
func TestNewMetricsServer(t *testing.T) {
	for _, tc := range []struct {
		name       string
		path       string
		wantStatus int
		wantBody   []string
	}{
		{
			// Rejection series exist at zero so alerts have a baseline before the first rejection.
			name:       "metrics path serves the registry",
			path:       metricsPath,
			wantStatus: http.StatusOK,
			wantBody: []string{
				`nico_mcp_credential_forwarding_rejected_total{reason="base_url_mismatch"} 0`,
				`nico_mcp_credential_forwarding_rejected_total{reason="inherited_credentials"} 0`,
				"go_goroutines ",
				"process_start_time_seconds ",
			},
		},
		// The metrics port must not become a second way to reach MCP tools.
		{name: "MCP path is not served", path: "/mcp", wantStatus: http.StatusNotFound},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ts := httptest.NewServer(newMetricsServer("", NewMetrics()).Handler)
			defer ts.Close()

			resp, err := http.Get(ts.URL + tc.path)
			require.NoError(t, err)
			defer resp.Body.Close()
			body, err := io.ReadAll(resp.Body)
			require.NoError(t, err)

			require.Equal(t, tc.wantStatus, resp.StatusCode)
			for _, want := range tc.wantBody {
				require.Contains(t, string(body), want)
			}
		})
	}
}

// TestNewServers protects the --metrics-listen contract: metrics get their own
// listener only when asked for, and the MCP listener never serves them.
func TestNewServers(t *testing.T) {
	for _, tc := range []struct {
		name        string
		args        []string
		env         map[string]string
		wantMetrics string
	}{
		{name: "no metrics listener by default"},
		{name: "flag adds a metrics listener", args: []string{"--metrics-listen", "127.0.0.1:9360"}, wantMetrics: "127.0.0.1:9360"},
		{name: "environment variable adds a metrics listener", env: map[string]string{"NICO_MCP_METRICS_LISTEN": ":9361"}, wantMetrics: ":9361"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			for key, value := range tc.env {
				t.Setenv(key, value)
			}
			var servers []*http.Server
			app := &urfave.App{
				Flags: ServeFlags(),
				Action: func(c *urfave.Context) error {
					opts := buildServeOptions(c)
					server, err := BuildServer([]byte(synthSpec), opts)
					if err != nil {
						return err
					}
					servers, err = newServers(c, server, opts)
					return err
				},
			}

			require.NoError(t, app.Run(append([]string{"nico-mcp"}, tc.args...)))

			if tc.wantMetrics == "" {
				require.Len(t, servers, 1)
			} else {
				require.Len(t, servers, 2)
				require.Equal(t, tc.wantMetrics, servers[1].Addr)
				recorder := httptest.NewRecorder()
				servers[1].Handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, metricsPath, nil))
				require.Equal(t, http.StatusOK, recorder.Code)
			}
			require.Equal(t, ":8080", servers[0].Addr)
			recorder := httptest.NewRecorder()
			servers[0].Handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, metricsPath, nil))
			require.Equal(t, http.StatusNotFound, recorder.Code)
		})
	}
}
