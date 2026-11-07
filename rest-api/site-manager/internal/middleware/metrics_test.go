// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/stretchr/testify/assert"
)

// TestMetrics checks the request metric names and labels that dashboards rely
// on. The path label is the route template. Unmatched routes aren't counted.
func TestMetrics(t *testing.T) {
	tests := []struct {
		name         string
		envNamespace string
		want         string
	}{
		{
			name: "metric names start with the given namespace",
			want: "nico_rest_site_manager",
		},
		{
			name:         "METRICS_NAMESPACE replaces the namespace",
			envNamespace: "acme_site_manager",
			want:         "acme_site_manager",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv(metricsNamespaceEnv, tt.envNamespace)

			// Metrics registers with the default registerer.
			registry := prometheus.NewRegistry()
			previousRegisterer := prometheus.DefaultRegisterer
			prometheus.DefaultRegisterer = registry
			t.Cleanup(func() { prometheus.DefaultRegisterer = previousRegisterer })

			e := echo.New()
			e.Add(http.MethodPost, "/v1/site/roll/:uuid", func(c echo.Context) error {
				return c.NoContent(http.StatusOK)
			}, Metrics("nico_rest_site_manager"))

			e.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodPost, "/v1/site/roll/site-uuid", nil))
			e.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/unknown", nil))

			metrics := httptest.NewRecorder()
			promhttp.HandlerFor(registry, promhttp.HandlerOpts{}).
				ServeHTTP(metrics, httptest.NewRequest(http.MethodGet, "/metrics", nil))
			body := metrics.Body.String()

			labels := `method="POST",path="/v1/site/roll/:uuid"`
			assert.Contains(t, body, fmt.Sprintf("%s_http_duration_seconds_count{%s} 1\n", tt.want, labels))
			for _, name := range []string{"http_request_counts", "http_requests_total"} {
				assert.Contains(t, body, fmt.Sprintf("%s_%s{%s,status_code=\"200\"} 1\n", tt.want, name, labels))
			}
			assert.NotContains(t, body, `path="/unknown"`)
		})
	}
}
