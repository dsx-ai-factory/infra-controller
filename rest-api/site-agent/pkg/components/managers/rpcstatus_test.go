// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// testRPCStatus exercises manager configuration, real RPC completion, and HTTP
// reporting together; no test calls UpdateGrpcClientState to simulate wiring.
func testRPCStatus(t *testing.T) {
	const metricsNamespace = "custom_site_agent"

	oldRegisterer, oldGatherer := prometheus.DefaultRegisterer, prometheus.DefaultGatherer
	registry := prometheus.NewRegistry()
	prometheus.DefaultRegisterer, prometheus.DefaultGatherer = registry, registry
	t.Cleanup(func() {
		prometheus.DefaultRegisterer, prometheus.DefaultGatherer = oldRegisterer, oldGatherer
	})
	checkHealthMetrics := func(t *testing.T, aggregate, temporal int) {
		t.Helper()
		response := httptest.NewRecorder()
		newMetricsServeMux().ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/metrics", nil))
		require.Equal(t, http.StatusOK, response.Code)
		for _, metric := range []struct {
			name  string
			kind  string
			help  string
			value int
		}{
			{
				name: "health_status", kind: "counter",
				help: "health status of the Site Agent", value: aggregate,
			},
			{
				name: "health", kind: "gauge",
				help: "Current aggregate health of the Site Agent: 0 unhealthy, 1 healthy", value: aggregate,
			},
			{
				name: "temporal_connection_status", kind: "counter",
				help: "Temporal health status of the Site Agent", value: temporal,
			},
			{
				name: "temporal_connection_health", kind: "gauge",
				help: "Current Temporal connection health of the Site Agent: 0 unhealthy, 1 healthy; 2 reserved for unknown", value: temporal,
			},
		} {
			name := metricsNamespace + "_" + metric.name
			assert.Contains(t, response.Body.String(), fmt.Sprintf("# HELP %s %s\n", name, metric.help))
			assert.Contains(t, response.Body.String(), fmt.Sprintf("# TYPE %s %s\n", name, metric.kind))
			assert.Contains(t, response.Body.String(), fmt.Sprintf("\n%s %d\n", name, metric.value))
		}
	}
	var responseCode atomic.Uint32
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	server := grpc.NewServer(grpc.UnaryInterceptor(func(ctx context.Context, req any, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (any, error) {
		code := codes.Code(responseCode.Load())
		if code != codes.OK {
			return nil, status.Error(code, "injected RPC failure")
		}
		return handler(ctx, req)
	}))
	corev1.RegisterForgeServer(server, &statusCoreServer{})
	flowv1.RegisterFlowServer(server, &statusFlowServer{})
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)

	elektra := elektratypes.NewElektraTypes()
	elektra.Conf.FlowGrpc.Enabled = true
	elektra.Conf.DisableBootstrap = true
	elektra.Conf.MetricsNamespace = metricsNamespace
	// Insecure clients do not parse certificates, but manager startup hashes them.
	certPath := filepath.Join(t.TempDir(), "certificate")
	require.NoError(t, os.WriteFile(certPath, []byte("test certificate hash input"), 0600))
	elektra.Conf.CoreGrpc.Address = listener.Addr().String()
	elektra.Conf.CoreGrpc.ClientCertPath = certPath
	elektra.Conf.CoreGrpc.ServerCAPath = certPath
	elektra.Conf.FlowGrpc.Address = listener.Addr().String()
	elektra.Conf.FlowGrpc.ClientCertPath = certPath
	elektra.Conf.FlowGrpc.ServerCAPath = certPath
	manager, err := NewInstance(elektra)
	require.NoError(t, err)
	manager.Init()
	checkHealthMetrics(t, 0, 0)
	elektra.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompHealthy))
	require.NoError(t, manager.API.CoreGrpc.CreateGrpcClient())
	require.NoError(t, manager.API.FlowGrpc.CreateGrpcClient())
	t.Cleanup(func() {
		assert.NoError(t, elektra.Managers.CoreGrpc.GetClient().Close())
		assert.NoError(t, elektra.Managers.FlowGrpc.GetClient().Close())
	})

	readStatus := func(t *testing.T) siteAgentStatus {
		t.Helper()
		response := httptest.NewRecorder()
		newStatusServeMux().ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/status", nil))
		require.Equal(t, http.StatusOK, response.Code)
		var status siteAgentStatus
		require.NoError(t, json.Unmarshal(response.Body.Bytes(), &status))
		return status
	}

	tests := []struct {
		name     string
		section  func(siteAgentStatus) *grpcStatus
		call     func(context.Context) error
		recreate func()
	}{
		{
			name: "Core", section: func(status siteAgentStatus) *grpcStatus { return &status.CoreGrpc },
			call: func(ctx context.Context) error {
				_, err := manager.API.CoreGrpc.GetGrpcClient().GrpcServiceClient().Version(ctx, &corev1.VersionRequest{})
				return err
			},
			recreate: func() {
				replacement, err := client.NewCoreGrpcClient(elektra.Managers.CoreGrpc.Client.Config)
				require.NoError(t, err)
				old := elektra.Managers.CoreGrpc.Client.SwapClient(replacement)
				require.NoError(t, old.Close())
			},
		},
		{
			name: "Flow", section: func(status siteAgentStatus) *grpcStatus { return status.FlowGrpc },
			call: func(ctx context.Context) error {
				_, err := manager.API.FlowGrpc.GetGrpcClient().GrpcServiceClient().Version(ctx, &flowv1.VersionRequest{})
				return err
			},
			recreate: func() {
				replacement, err := client.NewFlowGrpcClient(elektra.Managers.FlowGrpc.Client.Config)
				require.NoError(t, err)
				old := elektra.Managers.FlowGrpc.Client.SwapClient(replacement)
				require.NoError(t, old.Close())
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			// Recreate using the retained config, as the certificate reload path does.
			tc.recreate()
			succeeded, failed := uint64(2), uint64(0) // Initial and replacement Version probes.
			for _, step := range []struct {
				name        string
				code        codes.Code
				health      string
				healthValue int
			}{
				{"success", codes.OK, "Healthy", 1},
				{"connection failure", codes.Unavailable, "Unhealthy", 0},
				{"recovery", codes.OK, "Healthy", 1},
				{"application failure", codes.InvalidArgument, "Healthy", 1},
			} {
				t.Run(step.name, func(t *testing.T) {
					responseCode.Store(uint32(step.code))
					ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
					defer cancel()
					rpcErr := tc.call(ctx)
					require.Equal(t, step.code, status.Code(rpcErr))
					if rpcErr == nil {
						succeeded++
					} else {
						failed++
					}
					status := readStatus(t)
					section := tc.section(status)
					require.NotNil(t, section)
					assert.Equal(t, succeeded, section.RequestsSucceeded)
					assert.Equal(t, failed, section.RequestsFailed)
					assert.Equal(t, step.health, section.Health)
					assert.Equal(t, step.health, status.Health)
					checkHealthMetrics(t, step.healthValue, 1)
				})
			}
			t.Run("concurrent calls and status reads", func(t *testing.T) {
				responseCode.Store(uint32(codes.Unavailable))
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				defer cancel()
				var wg sync.WaitGroup
				for range 8 {
					wg.Go(func() {
						assert.Equal(t, codes.Unavailable, status.Code(tc.call(ctx)))
					})
					wg.Go(func() {
						response := httptest.NewRecorder()
						newStatusServeMux().ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/status", nil))
						assert.Equal(t, http.StatusOK, response.Code)
					})
				}
				wg.Wait()
				status := readStatus(t)
				assert.Equal(t, failed+8, tc.section(status).RequestsFailed)
				assert.Equal(t, "Unhealthy", status.Health)
				responseCode.Store(uint32(codes.OK))
				require.NoError(t, tc.call(ctx))
			})
			responseCode.Store(uint32(codes.OK))
		})
	}
	t.Run("Temporal health transitions", func(t *testing.T) {
		require.Equal(t, uint64(1), elektra.Managers.CoreGrpc.State.HealthStatus.Load())
		require.Equal(t, uint64(1), elektra.Managers.FlowGrpc.State.HealthStatus.Load())
		for _, step := range []struct {
			name      string
			status    computils.CompStatus
			aggregate int
			temporal  int
		}{
			{"healthy", computils.CompHealthy, 1, 1},
			{"unhealthy", computils.CompUnhealthy, 0, 0},
			{"recovered", computils.CompHealthy, 1, 1},
			{"reserved unknown", computils.CompNotKnown, 0, 2},
		} {
			t.Run(step.name, func(t *testing.T) {
				elektra.Managers.Workflow.State.HealthStatus.Store(uint64(step.status))
				checkHealthMetrics(t, step.aggregate, step.temporal)
			})
		}
	})
}

type statusCoreServer struct {
	corev1.UnimplementedForgeServer
}

func (*statusCoreServer) Version(context.Context, *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{}, nil
}

type statusFlowServer struct{ flowv1.UnimplementedFlowServer }

func (*statusFlowServer) Version(context.Context, *flowv1.VersionRequest) (*flowv1.BuildInfo, error) {
	return &flowv1.BuildInfo{}, nil
}
