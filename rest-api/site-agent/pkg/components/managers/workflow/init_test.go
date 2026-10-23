// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"net/http"
	"net/http/httptest"
	"testing"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestAPI_Init verifies that initialization resets prior health and retains the default metric families.
func TestAPI_Init(t *testing.T) {
	previousRegisterer := prometheus.DefaultRegisterer
	previousGauge := CertExpirationMetric
	t.Cleanup(func() {
		prometheus.DefaultRegisterer = previousRegisterer
		CertExpirationMetric = previousGauge
	})
	registry := prometheus.NewRegistry()
	prometheus.DefaultRegisterer = registry
	data := newHealthTestManager(t, &conftypes.Config{MetricsNamespace: conftypes.DefaultMetricsNamespace})
	state := data.Managers.Workflow.State
	state.HealthStatus.Store(uint64(computils.CompHealthy))
	(&API{}).Init()
	require.Equal(t, uint64(0), state.HealthStatus.Load())

	response := httptest.NewRecorder()
	handler := promhttp.HandlerFor(registry, promhttp.HandlerOpts{})
	handler.ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/metrics", nil))
	require.Equal(t, http.StatusOK, response.Code)
	body := response.Body.String()
	for _, metric := range []struct {
		name string
		kind string
	}{
		{"temporal_connection_status", "counter"},
		{"temporal_connection_health", "gauge"},
		{"temporal_connection_attempted", "counter"},
		{"temporal_connection_succeeded", "counter"},
		{"temporal_cert_expiration_timestamp_seconds", "gauge"},
	} {
		assert.Contains(t, body, "# TYPE nico_rest_site_agent_"+metric.name+" "+metric.kind+"\n")
	}
	assert.Contains(t, body, "\nnico_rest_site_agent_temporal_connection_status 0\n")
	assert.Contains(t, body, "\nnico_rest_site_agent_temporal_connection_health 0\n")
}
