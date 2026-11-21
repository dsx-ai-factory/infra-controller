// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
)

func TestInvokeGRPCProxyActivityDeadlinePrecedesWorkflowTimeout(t *testing.T) {
	cases := []struct {
		name         string
		workflowFn   any
		activityName string
		fullMethod   string
	}{
		{
			name:         "core",
			workflowFn:   InvokeCoreGRPC,
			activityName: "InvokeCoreGRPCOnSite",
			fullMethod:   "/forge.Forge/Test",
		},
		{
			name:         "flow",
			workflowFn:   InvokeFlowGRPC,
			activityName: "InvokeFlowGRPCOnSite",
			fullMethod:   "/v1.Flow/Version",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			var activityDeadline time.Time
			var hasActivityDeadline bool

			env.RegisterActivityWithOptions(
				func(ctx context.Context, _ grpcproxy.Request) (grpcproxy.Response, error) {
					activityDeadline, hasActivityDeadline = ctx.Deadline()
					return grpcproxy.Response{}, nil
				},
				sdkactivity.RegisterOptions{Name: tc.activityName},
			)

			env.ExecuteWorkflow(tc.workflowFn, grpcproxy.Request{FullMethod: tc.fullMethod})

			require.True(t, env.IsWorkflowCompleted())
			require.NoError(t, env.GetWorkflowError())
			require.True(t, hasActivityDeadline)

			// The deadline must track ActivityStartToCloseTimeout rather than
			// any fixed duration, so tuning the ladder does not require
			// editing this test.
			remaining := time.Until(activityDeadline)
			assert.Greater(t, remaining, grpcproxy.ActivityStartToCloseTimeout-time.Second)
			assert.LessOrEqual(t, remaining, grpcproxy.ActivityStartToCloseTimeout)
		})
	}
}

func TestProtectedProxyWorkflows(t *testing.T) {
	for _, tc := range []struct {
		name         string
		workflowFn   any
		activityName string
		missing      bool
	}{
		{"Core forwards opaque envelopes", InvokeCoreGRPCProtected, "InvokeCoreGRPCProtectedOnSite", false},
		{"Flow forwards opaque envelopes", InvokeFlowGRPCProtected, "InvokeFlowGRPCProtectedOnSite", false},
		{"missing envelope rejected", InvokeCoreGRPCProtected, "InvokeCoreGRPCProtectedOnSite", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			req := grpcproxy.Request{FullMethod: "Test", ProtectedSecrets: &grpcproxy.ProtectedPayload{Version: 1, Ciphertext: "opaque-request"}}
			reply := grpcproxy.Response{ProtectedResponse: &grpcproxy.ProtectedPayload{Version: 1, Ciphertext: "opaque-response"}}
			if tc.missing {
				req.ProtectedSecrets = nil
			}
			calls := 0
			env.RegisterActivityWithOptions(func(_ context.Context, r grpcproxy.Request) (grpcproxy.Response, error) {
				calls++
				require.Equal(t, req, r)
				return reply, nil
			}, sdkactivity.RegisterOptions{Name: tc.activityName})
			env.ExecuteWorkflow(tc.workflowFn, req)
			if tc.missing {
				require.Error(t, env.GetWorkflowError())
				require.Zero(t, calls)
				return
			}
			require.NoError(t, env.GetWorkflowError())
			var got grpcproxy.Response
			require.NoError(t, env.GetWorkflowResult(&got))
			require.Equal(t, reply, got)
			require.Equal(t, 1, calls)
		})
	}
}
