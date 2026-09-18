// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// newProxyEchoContext returns a context for the helpers that render their own
// response, along with the recorder holding what they wrote.
func newProxyEchoContext() (echo.Context, *httptest.ResponseRecorder) {
	recorder := httptest.NewRecorder()
	return echo.New().NewContext(httptest.NewRequest(http.MethodPost, "/", nil), recorder), recorder
}

// startedProxyWorkflow captures what a proxy helper handed to Temporal.
type startedProxyWorkflow struct {
	options      tclient.StartWorkflowOptions
	workflowName string
}

// newTimingOutProxyClient returns a Temporal client whose workflow hits its
// execution timeout, so helpers take their timeout path.
func newTimingOutProxyClient() (*tmocks.Client, *startedProxyWorkflow) {
	return newProxyClient(tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), nil)
}

// newProxyClient returns a Temporal client whose start fails with startErr, or,
// when that is nil, whose workflow result is getErr.
func newProxyClient(getErr, startErr error) (*tmocks.Client, *startedProxyWorkflow) {
	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Return(getErr)

	started := &startedProxyWorkflow{}
	temporalClient := &tmocks.Client{}
	temporalClient.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.MatchedBy(func(options tclient.StartWorkflowOptions) bool {
			started.options = options
			return true
		}),
		mock.Anything,
		mock.Anything,
	).Run(func(args mock.Arguments) {
		started.workflowName = args.Get(2).(string)
	}).Return(workflowRun, startErr)

	return temporalClient, started
}

// assertNoTermination fails if the proxy tried to terminate the execution. It
// matches on the method rather than an argument list so a call that passes
// termination details cannot slip past.
func assertNoTermination(t *testing.T, temporalClient *tmocks.Client) {
	t.Helper()

	for _, call := range temporalClient.Calls {
		assert.NotEqual(t, "TerminateWorkflow", call.Method, "the proxy must leave the execution alone")
	}
}

// TestExecuteGRPCProxyClassifiesLostResults separates the ways a caller ends up
// without a result. They all answer 504, so only the internal cause tells an
// operator whether the execution closed itself or is still running unobserved.
//
// Neither case terminates: the activity does not heartbeat, so termination
// would discard the result without stopping the RPC it is blocked on, and for a
// deterministic ID it would free the name for a retry to start a duplicate.
func TestExecuteGRPCProxyClassifiesLostResults(t *testing.T) {
	cases := []struct {
		name          string
		getErr        error
		startErr      error
		cancelCaller  bool
		expectedCode  int
		expectedMsg   string
		expectedCause string
	}{
		{
			name:          "temporal timeout while the caller is still waiting",
			getErr:        tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil),
			expectedCode:  http.StatusGatewayTimeout,
			expectedMsg:   "Flow proxy request timed out",
			expectedCause: "execution timed out",
		},
		{
			name:          "caller stops waiting for the result",
			getErr:        context.Canceled,
			cancelCaller:  true,
			expectedCode:  http.StatusGatewayTimeout,
			expectedMsg:   "Flow proxy request timed out",
			expectedCause: "caller stopped waiting",
		},
		{
			name:          "caller stops waiting during the start",
			startErr:      context.Canceled,
			cancelCaller:  true,
			expectedCode:  http.StatusGatewayTimeout,
			expectedMsg:   "Flow proxy request timed out",
			expectedCause: "caller stopped waiting",
		},
		{
			// The SDK's own start deadline is far shorter than the caller's
			// budget, so a slow frontend runs out of time while the caller is
			// still waiting. The start may have landed all the same.
			name:          "start exceeds the SDK deadline",
			startErr:      serviceerror.NewDeadlineExceeded("context deadline exceeded"),
			expectedCode:  http.StatusGatewayTimeout,
			expectedMsg:   "Flow proxy request timed out",
			expectedCause: "start deadline exceeded",
		},
		{
			// The execution's own deadline is not the caller's, so it stays a
			// workflow failure rather than being reported as a gateway timeout.
			name:         "deadline exceeded from inside a live execution",
			getErr:       context.DeadlineExceeded,
			expectedCode: http.StatusInternalServerError,
			expectedMsg:  context.DeadlineExceeded.Error(),
		},
		{
			name:          "start fails while the caller is still waiting",
			startErr:      errors.New("namespace not found"),
			expectedCode:  http.StatusInternalServerError,
			expectedMsg:   "Failed to execute Flow proxy workflow",
			expectedCause: "namespace not found",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			temporalClient, _ := newProxyClient(tc.getErr, tc.startErr)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if tc.cancelCaller {
				cancel()
			}

			err := ExecuteFlowGRPC(ctx, temporalClient, "/v1.Flow/CreateOperationRun", &emptypb.Empty{}, nil, "create-1", temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, "")

			require.NotNil(t, err)
			assert.Equal(t, tc.expectedCode, err.Code)
			assert.Equal(t, tc.expectedMsg, err.Message)
			assertNoTermination(t, temporalClient)
			if tc.expectedCause == "" {
				return
			}
			require.NotNil(t, err.Data)
			cause, ok := err.Data.(error)
			require.True(t, ok, "Data is %T", err.Data)
			assert.Contains(t, cause.Error(), tc.expectedCause)
		})
	}
}

// TestProxyFlowGRPCSeparatesDiagnosisFromResponseData keeps the diagnosis in
// the log and out of the body: an error placed in the response serializes to an
// empty object, which tells a client nothing and contradicts the null the
// schema documents.
func TestProxyFlowGRPCSeparatesDiagnosisFromResponseData(t *testing.T) {
	cases := []struct {
		name         string
		getErr       error
		expectedCode int
		expectedLog  string
	}{
		{
			// Flow's own rejection arrives with no separate cause, so the
			// message is the only diagnosis there is.
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
			temporalClient, _ := newProxyClient(tc.getErr, nil)
			echoCtx, recorder := newProxyEchoContext()
			var logs bytes.Buffer

			err := ProxyFlowGRPC(
				context.Background(), echoCtx, zerolog.New(&logs), temporalClient,
				"/v1.Flow/GetRackInfoByID",
				&emptypb.Empty{}, nil,
				"rack-get-1", temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
			)

			require.NoError(t, err)
			assert.Equal(t, tc.expectedCode, recorder.Code)
			assert.Contains(t, logs.String(), tc.expectedLog)

			var body map[string]any
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &body))
			require.Contains(t, body, "data")
			assert.Nil(t, body["data"], "data must be null, got %#v", body["data"])
		})
	}
}

func TestExecuteCoreGRPC(t *testing.T) {
	temporalClient, started := newTimingOutProxyClient()

	err := ExecuteCoreGRPC(
		context.Background(),
		temporalClient,
		"/forge.Forge/Test",
		&emptypb.Empty{},
		nil,
		"",
	)

	t.Run("starts the Core proxy workflow", func(t *testing.T) {
		assert.Equal(t, grpcproxy.Core.WorkflowName, started.workflowName)
	})

	t.Run("generates a fresh workflow ID so identical calls never coalesce", func(t *testing.T) {
		assert.True(t, strings.HasPrefix(started.options.ID, "core-grpc-Test-"), "got %q", started.options.ID)
		assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, started.options.WorkflowIDConflictPolicy)
	})

	t.Run("expires before the caller gives up", func(t *testing.T) {
		assert.Equal(t, grpcproxy.WorkflowExecutionTimeout, started.options.WorkflowExecutionTimeout)
		assert.Less(t, started.options.WorkflowExecutionTimeout, cutil.WorkflowContextTimeout)
	})

	t.Run("reports a timeout as 504", func(t *testing.T) {
		require.NotNil(t, err)
		assert.Equal(t, http.StatusGatewayTimeout, err.Code)
		assert.Equal(t, "Core proxy request timed out", err.Message)
	})
}

// TestExecuteGRPCProxyRejectsSecretFieldsWithoutKey covers the combination that
// would otherwise put a secret into Temporal history in cleartext: the caller
// named fields to redact but gave no key to encrypt them with.
func TestExecuteGRPCProxyRejectsSecretFieldsWithoutKey(t *testing.T) {
	cases := []struct {
		name    string
		execute func(tclient.Client) *cutil.APIError
		message string
	}{
		{
			name: "core",
			execute: func(stc tclient.Client) *cutil.APIError {
				return ExecuteCoreGRPC(context.Background(), stc, "/forge.Forge/RotateCredential", &emptypb.Empty{}, nil, "", "password")
			},
			message: "Failed to encode Core proxy request",
		},
		{
			name: "flow",
			execute: func(stc tclient.Client) *cutil.APIError {
				return ExecuteFlowGRPC(context.Background(), stc, "/v1.Flow/CreateOperationRun", &emptypb.Empty{}, nil, "create-1", temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, "", "password")
			},
			message: "Failed to encode Flow proxy request",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			temporalClient := &tmocks.Client{}

			err := tc.execute(temporalClient)

			require.NotNil(t, err)
			assert.Equal(t, http.StatusInternalServerError, err.Code)
			assert.Equal(t, tc.message, err.Message)
			temporalClient.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
		})
	}
}

func TestExecuteFlowGRPC(t *testing.T) {
	temporalClient, started := newTimingOutProxyClient()

	const workflowID = "get-operation-run-run-1-true"
	err := ExecuteFlowGRPC(
		context.Background(),
		temporalClient,
		"/v1.Flow/GetOperationRun",
		&emptypb.Empty{},
		nil,
		workflowID,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
		"",
	)

	t.Run("starts the Flow proxy workflow", func(t *testing.T) {
		assert.Equal(t, grpcproxy.Flow.WorkflowName, started.workflowName)
	})

	t.Run("passes the caller's ID and conflict policy through", func(t *testing.T) {
		assert.Equal(t, workflowID, started.options.ID)
		assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING, started.options.WorkflowIDConflictPolicy)
		assert.Equal(t, temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE, started.options.WorkflowIDReusePolicy)
	})

	t.Run("expires before the caller gives up", func(t *testing.T) {
		assert.Equal(t, grpcproxy.WorkflowExecutionTimeout, started.options.WorkflowExecutionTimeout)
		assert.Less(t, started.options.WorkflowExecutionTimeout, cutil.WorkflowContextTimeout)
	})

	t.Run("reports a timeout as 504", func(t *testing.T) {
		require.NotNil(t, err)
		assert.Equal(t, http.StatusGatewayTimeout, err.Code)
		assert.Equal(t, "Flow proxy request timed out", err.Message)
	})
}
