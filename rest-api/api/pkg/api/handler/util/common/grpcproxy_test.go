// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func newProxyClient(getErr error) *tmocks.Client {
	run := &tmocks.WorkflowRun{}
	run.On("Get", mock.Anything, mock.Anything).Return(getErr)
	client := &tmocks.Client{}
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(run, nil)
	return client
}

func TestProxyFlowGRPC(t *testing.T) {
	cases := []struct {
		name         string
		getErr       error
		expectedCode int
		expectedLog  string
	}{
		{
			name: "flow succeeds",
		},
		{
			name:         "flow rejects the request",
			getErr:       errors.New("rack not found"),
			expectedCode: http.StatusInternalServerError,
			expectedLog:  "rack not found",
		},
		{
			name:         "execution times out",
			getErr:       tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil),
			expectedCode: http.StatusGatewayTimeout,
			expectedLog:  "execution timed out",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			temporalClient := newProxyClient(tc.getErr)
			var logs bytes.Buffer

			apiErr := ProxyFlowGRPC(
				context.Background(), zerolog.New(&logs), temporalClient,
				"/v1.Flow/GetRackInfoByID",
				&emptypb.Empty{}, nil,
				"rack-get-1", temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
			)

			if tc.getErr == nil {
				assert.Nil(t, apiErr)
				assert.Empty(t, logs.String())
				return
			}

			require.NotNil(t, apiErr)
			assert.Equal(t, tc.expectedCode, apiErr.Code)
			assert.Contains(t, apiErr.Diagnosis().Error(), tc.expectedLog)
			assert.Contains(t, logs.String(), tc.expectedLog)
		})
	}
}

func TestExecuteCoreGRPC(t *testing.T) {
	testProxyDelegation(t, grpcproxy.Core.WorkflowName, func(ctx context.Context, client tclient.Client, request, response proto.Message) *cutil.APIError {
		return ExecuteCoreGRPC(ctx, client, "/wrapper.Service/Call", request, response, "wrapper-key", "secret")
	})
}

func TestExecuteFlowGRPC(t *testing.T) {
	testProxyDelegation(t, grpcproxy.Flow.WorkflowName, func(ctx context.Context, client tclient.Client, request, response proto.Message) *cutil.APIError {
		return ExecuteFlowGRPC(ctx, client, "/wrapper.Service/Call", request, response,
			"wrapper-flow", temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, "wrapper-key", "secret")
	})
}

// These checks protect wrapper argument forwarding; transport edge cases live
// with the shared siteproxy package.
func testProxyDelegation(t *testing.T, workflowName string, execute func(context.Context, tclient.Client, proto.Message, proto.Message) *cutil.APIError) {
	t.Helper()
	for _, tc := range []struct {
		name      string
		resultErr error
	}{
		{"successful response", nil},
		{"transport error", errors.New("transport failed")},
	} {
		t.Run(tc.name, func(t *testing.T) {
			type contextKey struct{}
			ctx := context.WithValue(context.Background(), contextKey{}, "caller")
			request, err := structpb.NewStruct(map[string]any{"public": "visible", "secret": "synthetic-secret"})
			require.NoError(t, err)
			response := &structpb.Struct{}
			client := &tmocks.Client{}
			run := &tmocks.WorkflowRun{}
			client.On("ExecuteWorkflow", mock.Anything, mock.Anything, workflowName, mock.Anything).
				Run(func(args mock.Arguments) {
					assert.Equal(t, "caller", args.Get(0).(context.Context).Value(contextKey{}))
					payload := args.Get(3).(grpcproxy.Request)
					assert.Equal(t, "/wrapper.Service/Call", payload.FullMethod)
					assert.JSONEq(t, `{"public":"visible","secret":"[REDACTED]"}`, string(payload.RequestJSON))
					require.NotEmpty(t, payload.EncryptedSecrets)
					assert.JSONEq(t, `{"secret":"synthetic-secret"}`, string(cutil.DecryptData(payload.EncryptedSecrets, "wrapper-key")))
					if workflowName == grpcproxy.Flow.WorkflowName {
						options := args.Get(1).(tclient.StartWorkflowOptions)
						assert.Equal(t, "wrapper-flow", options.ID)
						assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, options.WorkflowIDConflictPolicy)
					}
				}).Return(run, nil).Once()
			run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
				args.Get(1).(*grpcproxy.Response).ResponseJSON = []byte(`{"accepted":true}`)
			}).Return(tc.resultErr).Once()

			apiErr := execute(ctx, client, request, response)
			if tc.resultErr == nil {
				require.Nil(t, apiErr)
				assert.Equal(t, map[string]any{"accepted": true}, response.AsMap())
			} else {
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusInternalServerError, apiErr.Code)
				assert.Same(t, tc.resultErr, apiErr.Unwrap())
			}
			client.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}

func TestExecuteCoreGRPCWithWorkflowID(t *testing.T) {
	for _, tc := range []struct {
		name      string
		resultErr error
		observed  bool
	}{
		{"known result", nil, true},
		{"missing history remains unknown", serviceerror.NewNotFound("history expired"), false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &tmocks.Client{}
			run := &tmocks.WorkflowRun{}
			client.On("GetWorkflow", mock.Anything, "wrapper-attempt", "").Return(run).Once()
			run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
				args.Get(1).(*grpcproxy.Response).ResponseJSON = []byte(`{"allDone":true}`)
			}).Return(tc.resultErr).Once()
			response := &corev1.AdminForceDeleteMachineResponse{}

			err, observed := ExecuteCoreGRPCWithWorkflowID(context.Background(), client,
				corev1.Forge_AdminForceDeleteMachine_FullMethodName,
				&corev1.AdminForceDeleteMachineRequest{HostQuery: "wrapper-host"}, response,
				"wrapper-attempt", false, "")

			if tc.resultErr == nil {
				require.Nil(t, err)
				assert.True(t, response.AllDone)
			} else {
				require.NotNil(t, err)
				assert.Equal(t, http.StatusServiceUnavailable, err.Code)
				assert.Same(t, tc.resultErr, err.Unwrap())
			}
			assert.Equal(t, tc.observed, observed)
			client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			client.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}
