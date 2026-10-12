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
	oldRegisterer, oldGatherer := prometheus.DefaultRegisterer, prometheus.DefaultGatherer
	registry := prometheus.NewRegistry()
	prometheus.DefaultRegisterer, prometheus.DefaultGatherer = registry, registry
	t.Cleanup(func() {
		prometheus.DefaultRegisterer, prometheus.DefaultGatherer = oldRegisterer, oldGatherer
	})
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
	elektra.Conf.MetricsNamespace = "nico_rest_site_agent"
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
				name   string
				code   codes.Code
				health string
			}{
				{"success", codes.OK, "Healthy"},
				{"connection failure", codes.Unavailable, "Unhealthy"},
				{"recovery", codes.OK, "Healthy"},
				{"application failure", codes.InvalidArgument, "Healthy"},
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
					metrics := httptest.NewRecorder()
					newMetricsServeMux().ServeHTTP(metrics, httptest.NewRequest(http.MethodGet, "/metrics", nil))
					health := computils.CompHealthy
					if step.health == "Unhealthy" {
						health = computils.CompUnhealthy
					}
					assert.Contains(t, metrics.Body.String(), fmt.Sprintf("\nnico_rest_site_agent_health_status %d\n", health))
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
