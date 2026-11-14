// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/interceptor"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/worker"
	"go.temporal.io/sdk/workflow"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestNewWorkflowMetricsInterceptor(t *testing.T) {
	tests := []struct {
		name       string
		failCode   codes.Code
		invalid    bool
		wantErr    bool
		wantOK     int
		wantFailed int
	}{
		{name: "successful activity", wantOK: 1},
		{name: "terminal RPC failure", failCode: codes.InvalidArgument, wantErr: true, wantFailed: 1},
		{name: "retry followed by success", failCode: codes.Internal, wantOK: 1, wantFailed: 1},
		{name: "validation fails before RPC", invalid: true, wantErr: true, wantFailed: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			previousAccess := ManagerAccess
			previousRegisterer, previousGatherer := prometheus.DefaultRegisterer, prometheus.DefaultGatherer
			t.Cleanup(func() {
				ManagerAccess = previousAccess
				prometheus.DefaultRegisterer, prometheus.DefaultGatherer = previousRegisterer, previousGatherer
			})
			registry := prometheus.NewRegistry()
			prometheus.DefaultRegisterer, prometheus.DefaultGatherer = registry, registry
			conf := &conftypes.Config{MetricsNamespace: "test_site_agent"}
			data := &elektratypes.Elektra{Managers: managertypes.NewManagerType(), Log: zerolog.Nop()}
			NewCoreGrpcManager(data, nil, &managerapi.ManagerConf{EB: conf}).Init()

			listener, err := net.Listen("tcp", "127.0.0.1:0")
			require.NoError(t, err)
			server := grpc.NewServer()
			service := &workflowMetricsServer{firstCode: tt.failCode}
			corev1.RegisterForgeServer(server, service)
			go func() { _ = server.Serve(listener) }()
			t.Cleanup(server.Stop)
			grpcClient, err := client.NewCoreGrpcClient(&client.CoreGrpcClientConfig{
				Address: listener.Addr().String(), ClientMetrics: makeGrpcClientMetrics(),
			})
			require.NoError(t, err)
			t.Cleanup(func() { assert.NoError(t, grpcClient.Close()) })
			data.Managers.CoreGrpc.Client.SwapClient(grpcClient)

			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.SetWorkerOptions(worker.Options{Interceptors: []interceptor.WorkerInterceptor{
				NewWorkflowMetricsInterceptor(data.Managers.CoreGrpc.State.WflowMetrics),
			}})
			activity := swa.NewManageVpcPrefix(data.Managers.CoreGrpc.Client)
			env.RegisterActivity(activity.CreateVpcPrefixOnSite)
			request := &corev1.VpcPrefixCreationRequest{Id: &corev1.VpcPrefixId{Value: "test-prefix"}}
			if tt.invalid {
				request.Id = nil
			}
			if tt.failCode == codes.Internal {
				// CRUD workflows deliberately disable retries. Exercise attempt
				// accounting with an explicit retry policy around the real activity.
				env.ExecuteWorkflow(retryVpcPrefixActivity, request)
			} else {
				env.ExecuteWorkflow(sww.CreateVpcPrefix, request)
			}
			require.True(t, env.IsWorkflowCompleted())
			if tt.wantErr {
				require.Error(t, env.GetWorkflowError())
			} else {
				require.NoError(t, env.GetWorkflowError())
			}

			// Use the same default registry and handler as the Site Agent endpoint.
			response := httptest.NewRecorder()
			promhttp.Handler().ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/metrics", nil))
			require.Equal(t, http.StatusOK, response.Code)
			body := response.Body.String()
			assert.Contains(t, body, "# TYPE test_site_agent_workflow_latency_seconds histogram\n")
			assert.Contains(t, body, "# TYPE test_site_agent_carbide_grpc_client_latency_seconds histogram\n")
			for _, observation := range []struct {
				status string
				count  int
			}{
				{"Success", tt.wantOK}, {"ActivityFailed", tt.wantFailed},
			} {
				series := fmt.Sprintf(`test_site_agent_workflow_latency_seconds_count{activity="CreateVpcPrefixOnSite",status="%s"}`, observation.status)
				if observation.count == 0 {
					assert.NotContains(t, body, series)
				} else {
					assert.Contains(t, body, fmt.Sprintf("\n%s %d\n", series, observation.count))
				}
			}
			for _, observation := range []struct {
				code  string
				count int
			}{
				{"Ok", tt.wantOK}, {tt.failCode.String(), tt.wantFailed},
			} {
				series := fmt.Sprintf(`test_site_agent_carbide_grpc_client_latency_seconds_count{grpc_method="/forge.Forge/CreateVpcPrefix",grpc_status_code="%s"}`, observation.code)
				if tt.invalid || observation.count == 0 {
					assert.NotContains(t, body, series)
				} else {
					assert.Contains(t, body, fmt.Sprintf("\n%s %d\n", series, observation.count))
				}
			}
			wantCalls := tt.wantOK + tt.wantFailed
			if tt.invalid {
				wantCalls = 0
			}
			assert.Equal(t, int32(wantCalls), service.calls.Load())
		})
	}
}

func retryVpcPrefixActivity(ctx workflow.Context, request *corev1.VpcPrefixCreationRequest) error {
	ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
		StartToCloseTimeout: time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 2},
	})
	var manager swa.ManageVpcPrefix
	return workflow.ExecuteActivity(ctx, manager.CreateVpcPrefixOnSite, request).Get(ctx, nil)
}

type workflowMetricsServer struct {
	corev1.UnimplementedForgeServer
	firstCode codes.Code
	calls     atomic.Int32
}

func (*workflowMetricsServer) Version(context.Context, *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{}, nil
}

func (s *workflowMetricsServer) CreateVpcPrefix(context.Context, *corev1.VpcPrefixCreationRequest) (*corev1.VpcPrefix, error) {
	if s.calls.Add(1) == 1 && s.firstCode != codes.OK {
		return nil, status.Error(s.firstCode, "test RPC failure")
	}
	return &corev1.VpcPrefix{}, nil
}
