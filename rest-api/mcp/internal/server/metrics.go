// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"context"
	"errors"
	"net"
	"net/http"
	"strconv"
	"time"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/prometheus/client_golang/prometheus/promhttp"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

const metricsPath = "/metrics"

// Reasons a tool call is rejected before any REST request is sent because it
// would forward a credential to a destination the server does not trust.
const (
	rejectionBaseURLMismatch      = "base_url_mismatch"
	rejectionInheritedCredentials = "inherited_credentials"
)

// REST request outcomes that have no HTTP status code.
const (
	restStatusTimeout  = "timeout"
	restStatusCanceled = "canceled"
	restStatusError    = "error"
)

// durationBuckets start at one millisecond because an in-cluster REST read
// takes a few milliseconds, and reach past toolReadTimeout so a REST read that
// uses its whole budget still lands in a finite bucket.
var durationBuckets = []float64{0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60}

// Metrics holds the Prometheus series nico-mcp records. Each instance owns its
// registry, so several servers can run in one process.
//
// The three duration histograms separate where time goes: the HTTP histogram
// covers a whole MCP request, the tool histogram covers one tool handler, and
// the REST histogram covers only the NICo REST request inside it.
type Metrics struct {
	registry *prometheus.Registry

	httpRequests *prometheus.CounterVec
	httpDuration *prometheus.HistogramVec

	toolCalls    *prometheus.CounterVec
	toolDuration *prometheus.HistogramVec

	restRequests *prometheus.CounterVec
	restDuration *prometheus.HistogramVec

	credentialRejections *prometheus.CounterVec
}

// NewMetrics registers the nico-mcp series, along with the Go runtime and
// process collectors, in a new registry.
func NewMetrics() *Metrics {
	m := &Metrics{
		registry: prometheus.NewRegistry(),
		httpRequests: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "nico_mcp_http_requests_total",
			Help: "HTTP requests received on the MCP endpoint, by request method and response status code.",
		}, []string{"method", "code"}),
		httpDuration: prometheus.NewHistogramVec(prometheus.HistogramOpts{
			Name:    "nico_mcp_http_request_duration_seconds",
			Help:    "Time to serve one HTTP request on the MCP endpoint, including any NICo REST request it makes.",
			Buckets: durationBuckets,
		}, []string{"method"}),
		toolCalls: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "nico_mcp_tool_calls_total",
			Help: "MCP tool calls handled, by tool and result (success or error).",
		}, []string{"tool", "result"}),
		toolDuration: prometheus.NewHistogramVec(prometheus.HistogramOpts{
			Name:    "nico_mcp_tool_call_duration_seconds",
			Help:    "Time to handle one MCP tool call, including its NICo REST request.",
			Buckets: durationBuckets,
		}, []string{"tool"}),
		restRequests: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "nico_mcp_rest_requests_total",
			Help: "Requests sent to NICo REST, by tool and HTTP status code. The status code is timeout, canceled, or error when no response was read.",
		}, []string{"tool", "status_code"}),
		restDuration: prometheus.NewHistogramVec(prometheus.HistogramOpts{
			Name:    "nico_mcp_rest_request_duration_seconds",
			Help:    "Time from sending one NICo REST request to reading its full response body, by tool.",
			Buckets: durationBuckets,
		}, []string{"tool"}),
		credentialRejections: prometheus.NewCounterVec(prometheus.CounterOpts{
			Name: "nico_mcp_credential_forwarding_rejected_total",
			Help: "Tool calls rejected before any NICo REST request because they would send a credential to an untrusted base URL, by reason.",
		}, []string{"reason"}),
	}
	m.registry.MustRegister(
		collectors.NewGoCollector(),
		collectors.NewProcessCollector(collectors.ProcessCollectorOpts{}),
		m.httpRequests, m.httpDuration,
		m.toolCalls, m.toolDuration,
		m.restRequests, m.restDuration,
		m.credentialRejections,
	)
	// Publish the rejection series at zero so a rate() or alert has a baseline
	// before the first rejection.
	for _, reason := range []string{rejectionBaseURLMismatch, rejectionInheritedCredentials} {
		m.credentialRejections.WithLabelValues(reason)
	}
	return m
}

// Handler serves the registry in the Prometheus text format.
func (m *Metrics) Handler() http.Handler {
	return promhttp.HandlerFor(m.registry, promhttp.HandlerOpts{})
}

// instrumentHTTP records every request served by the MCP endpoint handler.
func (m *Metrics) instrumentHTTP(next http.Handler) http.Handler {
	return promhttp.InstrumentHandlerDuration(m.httpDuration,
		promhttp.InstrumentHandlerCounter(m.httpRequests, next))
}

// toolCallMiddleware records every tools/call request for a registered tool,
// including one the SDK rejects for invalid arguments before the tool handler
// runs. That rejection is a JSON-RPC error inside an HTTP 200 response, so
// only this counter shows it. A call naming any other tool is not recorded,
// so caller input never becomes a tool label.
func (m *Metrics) toolCallMiddleware(tools map[string]bool) mcp.Middleware {
	return func(next mcp.MethodHandler) mcp.MethodHandler {
		return func(ctx context.Context, method string, req mcp.Request) (mcp.Result, error) {
			call, ok := req.(*mcp.CallToolRequest)
			if !ok || !tools[call.Params.Name] {
				return next(ctx, method, req)
			}
			start := time.Now()
			result, err := next(ctx, method, req)
			toolResult, _ := result.(*mcp.CallToolResult)
			m.observeToolCall(call.Params.Name, err != nil || toolResult == nil || toolResult.IsError, time.Since(start))
			return result, err
		}
	}
}

// observeToolCall records one finished tool call. tool must be a registered
// tool name, never caller input.
func (m *Metrics) observeToolCall(tool string, isError bool, duration time.Duration) {
	result := "success"
	if isError {
		result = "error"
	}
	m.toolCalls.WithLabelValues(tool, result).Inc()
	m.toolDuration.WithLabelValues(tool).Observe(duration.Seconds())
}

// observeRESTRequest records one request the REST client sent for tool.
func (m *Metrics) observeRESTRequest(tool string, event appcli.RequestEvent) {
	m.restRequests.WithLabelValues(tool, restStatusLabel(event)).Inc()
	m.restDuration.WithLabelValues(tool).Observe(event.Duration.Seconds())
}

// observeConfigError counts err when it is a credential forwarding rejection.
func (m *Metrics) observeConfigError(err error) {
	switch {
	case errors.Is(err, errBaseURLMismatch):
		m.credentialRejections.WithLabelValues(rejectionBaseURLMismatch).Inc()
	case errors.Is(err, errInheritedCredentials):
		m.credentialRejections.WithLabelValues(rejectionInheritedCredentials).Inc()
	}
}

func restStatusLabel(event appcli.RequestEvent) string {
	// A dial or read timeout is a net.Error that does not match
	// context.DeadlineExceeded.
	var netErr net.Error
	switch {
	case event.StatusCode != 0:
		return strconv.Itoa(event.StatusCode)
	case errors.Is(event.Err, context.DeadlineExceeded):
		return restStatusTimeout
	case errors.Is(event.Err, context.Canceled):
		return restStatusCanceled
	case errors.As(event.Err, &netErr) && netErr.Timeout():
		return restStatusTimeout
	default:
		return restStatusError
	}
}

// newMetricsServer serves only the metrics path on its own listener, so the
// MCP port never exposes metrics and the metrics port never exposes tools.
func newMetricsServer(listen string, metrics *Metrics) *http.Server {
	mux := http.NewServeMux()
	mux.Handle(metricsPath, metrics.Handler())
	return &http.Server{
		Addr:              listen,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       30 * time.Second,
		WriteTimeout:      30 * time.Second,
		IdleTimeout:       120 * time.Second,
	}
}
