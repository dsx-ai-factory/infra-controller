// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"net/http"
	"net/http/httptest"
	"testing"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestHandleSiteStatusRequest(t *testing.T) {
	t.Run("real RPC transitions", testRPCStatus)
	tests := []struct {
		name            string
		flowGrpcEnabled bool
		expectFlowState bool
	}{
		{
			name:            "Flow gRPC enabled",
			flowGrpcEnabled: true,
			expectFlowState: true,
		},
		{
			name:            "Flow gRPC disabled",
			flowGrpcEnabled: false,
			expectFlowState: false,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			elektra := elektratypes.NewElektraTypes()
			elektra.Conf.FlowGrpc.Enabled = test.flowGrpcEnabled
			elektra.Managers.CoreGrpc.State.GrpcSucc.Store(3)
			elektra.Managers.FlowGrpc.State.GrpcSucc.Store(5)
			elektra.Managers.FlowGrpc.State.GrpcFail.Store(2)
			elektra.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))
			elektra.Managers.FlowGrpc.State.Err.Store("last Flow error")
			_, err := NewInstance(elektra)
			require.NoError(t, err)

			request := httptest.NewRequest(http.MethodGet, computils.SiteStatus, nil)
			response := httptest.NewRecorder()
			newStatusServeMux().ServeHTTP(response, request)

			assert.Equal(t, http.StatusOK, response.Code)
			assert.Contains(t, response.Body.String(), " GRPC Succeeded: 3\n")
			flowState := []string{
				" Flow GRPC Succeeded: 5\n",
				" Flow GRPC Failed: 2\n",
				" Flow GRPC Status: Healthy\n",
				" Flow GRPC Last Error: last Flow error\n",
			}
			for _, state := range flowState {
				if test.expectFlowState {
					assert.Contains(t, response.Body.String(), state)
				} else {
					assert.NotContains(t, response.Body.String(), state)
				}
			}
		})
	}
}

func TestNewStatusServeMux(t *testing.T) {
	statusPaths := []string{
		computils.SiteStatus,
		computils.VPCStatus,
		computils.SubnetStatus,
		computils.InstanceStatus,
		computils.MachineStatus,
	}
	mux := newStatusServeMux()
	for _, path := range statusPaths {
		t.Run("registers "+path, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, path, nil)
			_, pattern := mux.Handler(request)
			assert.Equal(t, path, pattern)
		})
	}

	excludedPaths := []string{"/metrics", "/unknown"}
	for _, path := range excludedPaths {
		t.Run("excludes "+path, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, path, nil)
			_, pattern := mux.Handler(request)
			assert.Empty(t, pattern)
		})
	}
}

func TestNewMetricsServeMux(t *testing.T) {
	mux := newMetricsServeMux()
	tests := []struct {
		name            string
		method          string
		wantStatus      int
		wantAllow       string
		wantBody        string
		wantCollections int
	}{
		{
			name:            "GET collects metrics",
			method:          http.MethodGet,
			wantStatus:      http.StatusOK,
			wantBody:        "site_agent_metrics_test 1\n",
			wantCollections: 1,
		},
		{
			name:       "POST is rejected before collection",
			method:     http.MethodPost,
			wantStatus: http.StatusMethodNotAllowed,
			wantAllow:  "GET, HEAD",
			wantBody:   "Method Not Allowed\n",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			collections := 0
			metric := prometheus.NewGaugeFunc(prometheus.GaugeOpts{
				Name: "site_agent_metrics_test",
				Help: "Metric used to verify Site Agent scrape dispatch.",
			}, func() float64 {
				collections++
				return 1
			})
			require.NoError(t, prometheus.Register(metric))
			t.Cleanup(func() { prometheus.Unregister(metric) })
			request := httptest.NewRequest(test.method, "/metrics", nil)
			response := httptest.NewRecorder()

			mux.ServeHTTP(response, request)

			assert.Equal(t, test.wantStatus, response.Code)
			assert.Equal(t, test.wantAllow, response.Header().Get("Allow"))
			assert.Contains(t, response.Body.String(), test.wantBody)
			assert.Equal(t, test.wantCollections, collections)
		})
	}

	excludedPaths := []string{
		computils.SiteStatus,
		computils.VPCStatus,
		computils.SubnetStatus,
		computils.InstanceStatus,
		computils.MachineStatus,
		"/unknown",
	}
	for _, path := range excludedPaths {
		t.Run("excludes "+path, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, path, nil)
			_, pattern := mux.Handler(request)
			assert.Empty(t, pattern)
		})
	}
}
