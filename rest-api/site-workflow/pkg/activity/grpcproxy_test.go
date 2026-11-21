// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	tclient "go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	proxytest "github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy/testutil"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestInvokeGRPCProxyOnSite(t *testing.T) {
	t.Run("managed envelopes", testInvokeGRPCProxyOnSiteProtected)
	t.Run("predecessor history", testInvokeGRPCProxyOnSiteLegacySecrets)
	cases := []struct {
		name         string
		err          error
		errType      string
		nonRetryable bool
	}{
		{
			name:         "missing local descriptor remains typed across Temporal",
			err:          fmt.Errorf("%w: FutureMethod", client.ErrUnknownProxyMethod),
			errType:      swe.ErrTypeNICoUnimplemented,
			nonRetryable: true,
		},
		{
			name:         "gRPC failed precondition remains typed across Temporal",
			err:          status.Error(codes.FailedPrecondition, "task cannot be cancelled"),
			errType:      swe.ErrTypeNICoFailedPrecondition,
			nonRetryable: true,
		},
		{name: "unrelated local error is not reported as unsupported", err: errors.New("invalid request JSON")},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := invokeGRPCProxyOnSite(context.Background(), grpcproxy.Core, "InvokeCoreGRPCOnSite",
				proxyErrorInvoker{err: tc.err}, "", grpcproxy.Request{FullMethod: "/forge.Forge/FutureMethod", RequestJSON: []byte(`{}`)})
			require.Error(t, err)

			// REST receives a deserialized Temporal error, not the original sentinel.
			converter := temporal.NewDefaultFailureConverter(temporal.DefaultFailureConverterOptions{})
			decoded := converter.FailureToError(converter.ErrorToFailure(err))
			var applicationErr *temporal.ApplicationError
			require.ErrorAs(t, decoded, &applicationErr)
			assert.Equal(t, tc.errType, applicationErr.Type())
			assert.Equal(t, tc.nonRetryable, applicationErr.NonRetryable())
		})
	}
}

func testInvokeGRPCProxyOnSiteProtected(t *testing.T) {
	for _, tc := range []struct {
		name                                 string
		backend                              grpcproxy.Backend
		relocate, backendError, replyFailure bool
	}{
		{name: "Core round trip", backend: grpcproxy.Core},
		{name: "Flow round trip", backend: grpcproxy.Flow},
		{name: "changed public target rejected before RPC", backend: grpcproxy.Core, relocate: true},
		{name: "backend error text never enters failure history", backend: grpcproxy.Core, backendError: true},
		{name: "reply encryption failure does not retry RPC", backend: grpcproxy.Core, replyFailure: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			proxytest.Configure(t)
			p, err := grpcproxy.NewVaultProtectorFromEnvironment()
			require.NoError(t, err)
			req := grpcproxy.Request{FullMethod: "/" + tc.backend.ServiceName + "/Test", RequestJSON: []byte(`{"password":"[REDACTED]","target":"machine-1"}`)}
			binding := grpcproxy.NewProtectionContext("site", tc.backend, "managed-workflow", req)
			req.ProtectedSecrets, err = p.Seal(context.Background(), binding, []byte(`{"password":"request-secret"}`))
			require.NoError(t, err)
			if tc.relocate {
				req.RequestJSON = []byte(`{"password":"[REDACTED]","target":"machine-2"}`)
			}
			calls := 0
			invoker := proxyJSONInvokerFunc(func(_ context.Context, method string, payload []byte) ([]byte, error) {
				calls++
				require.Equal(t, req.FullMethod, method)
				require.JSONEq(t, `{"password":"request-secret","target":"machine-1"}`, string(payload))
				if tc.backendError {
					return nil, status.Error(codes.InvalidArgument, "invalid password request-secret")
				}
				if tc.replyFailure {
					require.NoError(t, os.WriteFile(os.Getenv("GRPC_PROXY_VAULT_TOKEN_FILE"), []byte("expired"), 0600))
				}
				return []byte(`{"password":"response-secret"}`), nil
			})
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.SetStartWorkflowOptions(tclient.StartWorkflowOptions{ID: "managed-workflow"})
			env.RegisterActivityWithOptions(func(ctx context.Context, r grpcproxy.Request) (grpcproxy.Response, error) {
				return invokeGRPCProxyOnSite(ctx, tc.backend, "proxy", invoker, "site", r)
			}, sdkactivity.RegisterOptions{Name: "proxy"})
			wf := func(ctx workflow.Context, r grpcproxy.Request) (grpcproxy.Response, error) {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: grpcproxy.ActivityStartToCloseTimeout, RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
				var response grpcproxy.Response
				err := workflow.ExecuteActivity(ctx, "proxy", r).Get(ctx, &response)
				return response, err
			}
			env.ExecuteWorkflow(wf, req)
			if tc.relocate || tc.backendError || tc.replyFailure {
				err := env.GetWorkflowError()
				require.Error(t, err)
				require.NotContains(t, err.Error(), "request-secret")
				if tc.relocate {
					require.Zero(t, calls)
				} else {
					require.Equal(t, 1, calls)
				}
				if tc.backendError {
					var app *temporal.ApplicationError
					require.ErrorAs(t, err, &app)
					require.Equal(t, swe.ErrTypeNICoInvalidArgument, app.Type())
				}
				return
			}
			require.NoError(t, env.GetWorkflowError())
			var response grpcproxy.Response
			require.NoError(t, env.GetWorkflowResult(&response))
			require.Empty(t, response.ResponseJSON)
			serialized, err := json.Marshal(response)
			require.NoError(t, err)
			require.NotContains(t, string(serialized), "response-secret")
			plain, err := p.Open(context.Background(), binding.Response(), response.ProtectedResponse)
			require.NoError(t, err)
			require.JSONEq(t, `{"password":"response-secret"}`, string(plain))
			require.Equal(t, 1, calls)
		})
	}
}

func testInvokeGRPCProxyOnSiteLegacySecrets(t *testing.T) {
	for _, tc := range []struct {
		name             string
		allow, malformed bool
		wantCalls        int
	}{
		{name: "legacy credentials denied by default"},
		{name: "explicit migration drains predecessor request", allow: true, wantCalls: 1},
		{name: "malformed predecessor ciphertext does not panic", allow: true, malformed: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("GRPC_PROXY_ALLOW_LEGACY_SECRETS", fmt.Sprint(tc.allow))
			req := grpcproxy.Request{FullMethod: "Test", RequestJSON: []byte(`{"password":"[REDACTED]"}`), EncryptedSecrets: cutil.EncryptData([]byte(`{"password":"legacy"}`), "site")}
			if tc.malformed {
				req.EncryptedSecrets = []byte{1}
			}
			calls := 0
			_, err := invokeGRPCProxyOnSite(context.Background(), grpcproxy.Core, "proxy", proxyJSONInvokerFunc(func(_ context.Context, _ string, data []byte) ([]byte, error) {
				calls++
				require.JSONEq(t, `{"password":"legacy"}`, string(data))
				return []byte(`{}`), nil
			}), "site", req)
			require.Equal(t, tc.wantCalls, calls)
			if tc.wantCalls == 0 {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

type proxyJSONInvokerFunc func(context.Context, string, []byte) ([]byte, error)

func (f proxyJSONInvokerFunc) InvokeJSON(ctx context.Context, method string, data []byte) ([]byte, error) {
	return f(ctx, method, data)
}

// proxyErrorInvoker supplies a local proxy failure without calling a backend.
type proxyErrorInvoker struct {
	err error
}

// InvokeJSON returns the failure configured by the test.
func (p proxyErrorInvoker) InvokeJSON(context.Context, string, []byte) ([]byte, error) {
	return nil, p.err
}
