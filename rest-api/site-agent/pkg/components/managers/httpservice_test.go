// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	bootstraptypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/bootstrap"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestHandleSiteStatusRequest(t *testing.T) {
	t.Run("real RPC transitions", testRPCStatus)

	const (
		siteID   = "d2f4b0c6-6f1e-4a0e-9f5a-0b6a6f4c1e77"
		credsURL = "https://sitemgr.nico-system.svc/v1/sitecreds"
		otp      = "8Nn5Qk0mVQqHqk2hXwfXQz1Yk5A="
	)
	enabledBootstrap := `{"enabled": true, "message": null,
		"credentialDownloadsAttempted": 1, "credentialDownloadsSucceeded": 1}`
	disabledBootstrap := func(message string) string {
		return fmt.Sprintf(`{"enabled": false, "message": %q,
			"credentialDownloadsAttempted": null, "credentialDownloadsSucceeded": null}`, message)
	}
	connectedState := `"health": "Healthy",
		"temporal": {"health": "Healthy", "connectionsAttempted": 2, "connectionsSucceeded": 1,
			"lastConnectionAttempt": "2026-10-06T20:50:00Z"},
		"coreGrpc": {"health": "Healthy", "requestsSucceeded": 120, "requestsFailed": 2}`
	unconnectedState := `"health": "Unhealthy",
		"temporal": {"health": "Unhealthy", "connectionsAttempted": 0, "connectionsSucceeded": 0,
			"lastConnectionAttempt": null},
		"coreGrpc": {"health": "Unhealthy", "requestsSucceeded": 0, "requestsFailed": 0}`
	flowGrpcState := `{"health": "Healthy", "requestsSucceeded": 5, "requestsFailed": 2}`

	tests := []struct {
		name             string
		podName          string
		isMasterPod      bool
		disableBootstrap bool
		// connected records Temporal and Core gRPC results, as after the first checks.
		connected bool
		// flowGrpcEnabled enables Flow gRPC and records its results too.
		flowGrpcEnabled bool
		wantPod         string
		wantBootstrap   string
	}{
		{
			name:          "master pod",
			podName:       "nico-rest-site-agent-0",
			isMasterPod:   true,
			connected:     true,
			wantPod:       `{"name": "nico-rest-site-agent-0", "role": "Master"}`,
			wantBootstrap: enabledBootstrap,
		},
		{
			name:          "follower pod",
			podName:       "nico-rest-site-agent-1",
			connected:     true,
			wantPod:       `{"name": "nico-rest-site-agent-1", "role": "Follower"}`,
			wantBootstrap: disabledBootstrap("Bootstrap only runs on the master pod"),
		},
		{
			name:             "master pod with bootstrap disabled",
			podName:          "nico-rest-site-agent-0",
			isMasterPod:      true,
			disableBootstrap: true,
			connected:        true,
			wantPod:          `{"name": "nico-rest-site-agent-0", "role": "Master"}`,
			wantBootstrap:    disabledBootstrap("Bootstrap is disabled by DISABLE_BOOTSTRAP"),
		},
		{
			name:          "master pod before connecting",
			podName:       "nico-rest-site-agent-0",
			isMasterPod:   true,
			wantPod:       `{"name": "nico-rest-site-agent-0", "role": "Master"}`,
			wantBootstrap: enabledBootstrap,
		},
		{
			name:            "master pod with Flow gRPC enabled",
			podName:         "nico-rest-site-agent-0",
			isMasterPod:     true,
			connected:       true,
			flowGrpcEnabled: true,
			wantPod:         `{"name": "nico-rest-site-agent-0", "role": "Master"}`,
			wantBootstrap:   enabledBootstrap,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := newHealthTestManager(t, &managerapi.ManagerAPI{})
			// The Temporal address, the registration, and the errors below are recorded so the
			// comparison proves the response leaves them out.
			conf := &conftypes.Config{
				PodName:          tt.podName,
				IsMasterPod:      tt.isMasterPod,
				DisableBootstrap: tt.disableBootstrap,
				Temporal:         conftypes.TemporalConfig{Host: "temporal.nico.svc", Port: "7233"},
				FlowGrpc:         conftypes.FlowGrpcConfig{Enabled: tt.flowGrpcEnabled},
			}
			data.Conf = conf
			ManagerAccess.Conf = &managerapi.ManagerConf{EB: conf}
			bootstrap := data.Managers.Bootstrap
			bootstrap.Config = &bootstraptypes.SecretConfig{UUID: siteID, OTP: otp, CredsURL: credsURL}
			bootstrap.State.DownloadAttempted.Store(1)
			bootstrap.State.DownloadSucceeded.Store(1)
			state := unconnectedState
			if tt.connected {
				state = connectedState
				temporal := data.Managers.Workflow.State
				temporal.HealthStatus.Store(uint64(computils.CompHealthy))
				temporal.ConnectionAttempted.Store(2)
				temporal.ConnectionSucc.Store(1)
				// Recorded in local time, reported in UTC.
				temporal.SetConnectionTime(time.Date(2026, time.October, 6, 13, 50, 0, 0, time.FixedZone("PDT", -7*60*60)))
				temporal.SetErr("dial tcp 10.0.5.12:7233: connect: connection refused")
				coreGrpc := data.Managers.CoreGrpc.State
				coreGrpc.HealthStatus.Store(uint64(computils.CompHealthy))
				coreGrpc.GrpcSucc.Store(120)
				coreGrpc.GrpcFail.Store(2)
				coreGrpc.Err.Store("rpc error: code = Unavailable desc = dial tcp 10.0.5.13:1079: connect: connection refused")
			}
			flow := "null"
			if tt.flowGrpcEnabled {
				flow = flowGrpcState
				flowGrpc := data.Managers.FlowGrpc.State
				flowGrpc.HealthStatus.Store(uint64(computils.CompHealthy))
				flowGrpc.GrpcSucc.Store(5)
				flowGrpc.GrpcFail.Store(2)
				flowGrpc.Err.Store("rpc error: code = Unavailable desc = dial tcp 10.0.5.14:11080: connect: connection refused")
			}

			response := httptest.NewRecorder()
			handleSiteStatusRequest(response, httptest.NewRequest(http.MethodGet, computils.SiteStatus, nil))

			assert.Equal(t, "application/json", response.Header().Get("Content-Type"))
			assert.JSONEq(t, `{"pod": `+tt.wantPod+`, "bootstrap": `+tt.wantBootstrap+`, `+state+`, "flowGrpc": `+flow+`}`,
				response.Body.String())
		})
	}
}

func TestNewStatusServeMux(t *testing.T) {
	statusPaths := []string{
		computils.SiteStatus,
		computils.LivenessStatus,
		computils.ReadinessStatus,
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
