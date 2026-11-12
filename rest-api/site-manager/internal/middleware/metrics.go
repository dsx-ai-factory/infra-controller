// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"os"
	"strconv"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

// metricsNamespaceEnv replaces the metric name prefix passed to Metrics. Site
// Manager doesn't read a config file, so an environment variable is the only
// override available to an operator.
const metricsNamespaceEnv = "METRICS_NAMESPACE"

// Metrics registers request metrics with the default Prometheus registry and
// records each request in them:
//
//   - <namespace>_http_duration_seconds{path,method}, a histogram
//   - <namespace>_http_requests_total{path,method,status_code}, a counter
//   - <namespace>_http_request_counts{path,method,status_code}, the same
//     counter under its legacy name, kept for existing dashboards and alerts
//
// path is the route template, such as /v1/site/:uuid. A non-empty
// METRICS_NAMESPACE replaces namespace. Register Metrics as route middleware,
// so 404 and 405 responses aren't counted.
func Metrics(namespace string) echo.MiddlewareFunc {
	if envNamespace := os.Getenv(metricsNamespaceEnv); envNamespace != "" {
		namespace = envNamespace
	}

	latency := promauto.NewHistogramVec(prometheus.HistogramOpts{
		Name: namespace + "_http_duration_seconds",
		Help: "Duration of HTTP requests.",
	}, []string{"path", "method"})

	var counts []*prometheus.CounterVec
	for _, name := range []string{"http_request_counts", "http_requests_total"} {
		counts = append(counts, promauto.NewCounterVec(prometheus.CounterOpts{
			Name: namespace + "_" + name,
			Help: "Counter of HTTP requests.",
		}, []string{"path", "method", "status_code"}))
	}

	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			start := time.Now()
			err := next(c)
			if err != nil {
				// Writes the error response, so its status is counted.
				c.Error(err)
			}

			path, method := c.Path(), c.Request().Method
			latency.WithLabelValues(path, method).Observe(time.Since(start).Seconds())
			statusCode := strconv.Itoa(c.Response().Status)
			for _, count := range counts {
				count.WithLabelValues(path, method, statusCode).Inc()
			}
			return err
		}
	}
}
