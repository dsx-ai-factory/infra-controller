// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"bytes"
	"context"
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
	temporalCommon "go.temporal.io/api/common/v1"
	temporalEnums "go.temporal.io/api/enums/v1"
	temporalFailure "go.temporal.io/api/failure/v1"
	"go.temporal.io/api/serviceerror"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// newProxyEchoContext returns a context and recorder for helpers whose HTTP
// boundary behavior is under test.
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
	var workflowRun tclient.WorkflowRun
	if startErr == nil {
		run := &tmocks.WorkflowRun{}
		run.On("Get", mock.Anything, mock.Anything).Return(getErr)
		workflowRun = run
	}

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
			assert.Contains(t, err.Data.Error(), tc.expectedCause)
		})
	}
}

// TestProxyFlowGRPCReturnsAPIError keeps HTTP response ownership at the
// handler boundary while retaining the internal diagnosis for logging.
func TestProxyFlowGRPCReturnsAPIError(t *testing.T) {
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
			temporalClient, _ := newProxyClient(tc.getErr, nil)
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
	cause := tp.NewApplicationError("method unavailable", "methodError")
	applicationErr := tp.NewNonRetryableApplicationError("Core rejected the method", swe.ErrTypeNICoUnimplemented, cause)
	converter := tp.GetDefaultFailureConverter()
	activityErr := converter.FailureToError(&temporalFailure.Failure{
		Message: "activity failed",
		Cause:   converter.ErrorToFailure(applicationErr),
		FailureInfo: &temporalFailure.Failure_ActivityFailureInfo{
			ActivityFailureInfo: &temporalFailure.ActivityFailureInfo{
				ActivityType: &temporalCommon.ActivityType{Name: "InvokeCoreGRPCOnSite"},
			},
		},
	})

	cases := []struct {
		name            string
		getErr          error
		expectedCode    int
		expectedMessage string
		checkCause      bool
	}{
		{
			name:            "execution timeout",
			getErr:          tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil),
			expectedCode:    http.StatusGatewayTimeout,
			expectedMessage: "Core proxy request timed out",
		},
		{
			name:            "retains Temporal errors without exporting response data",
			getErr:          activityErr,
			expectedCode:    http.StatusNotImplemented,
			expectedMessage: cause.Error(),
			checkCause:      true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			temporalClient, started := newProxyClient(tc.getErr, nil)
			apiErr := ExecuteCoreGRPC(context.Background(), temporalClient, "/forge.Forge/Test", &emptypb.Empty{}, nil, "")

			assert.Equal(t, grpcproxy.Core.WorkflowName, started.workflowName)
			assert.True(t, strings.HasPrefix(started.options.ID, "core-grpc-Test-"), "got %q", started.options.ID)
			assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, started.options.WorkflowIDConflictPolicy)
			assert.Equal(t, grpcproxy.WorkflowExecutionTimeout, started.options.WorkflowExecutionTimeout)
			assert.Less(t, started.options.WorkflowExecutionTimeout, cutil.WorkflowContextTimeout)
			require.NotNil(t, apiErr)
			assert.Equal(t, tc.expectedCode, apiErr.Code)
			assert.Equal(t, tc.expectedMessage, apiErr.Message)
			if !tc.checkCause {
				return
			}

			assert.Same(t, tc.getErr, apiErr.Unwrap())
			var originalActivity *tp.ActivityError
			require.ErrorAs(t, apiErr, &originalActivity)
			assert.Equal(t, "InvokeCoreGRPCOnSite", originalActivity.ActivityType().Name)
			var originalApplication *tp.ApplicationError
			require.ErrorAs(t, apiErr, &originalApplication)
			assert.Equal(t, swe.ErrTypeNICoUnimplemented, originalApplication.Type())
			assert.EqualError(t, originalApplication.Unwrap(), cause.Error())
			assert.Nil(t, apiErr.Data)

			ctx, recorder := newProxyEchoContext()
			require.NoError(t, apiErr.Send(ctx))
			assert.Equal(t, tc.expectedCode, recorder.Code)
			assert.JSONEq(t, `{"source":"","message":"method unavailable (type: methodError, retryable: true)","data":null}`, recorder.Body.String())
		})
	}
}

func TestExecuteCoreGRPCWithConflictPolicy(t *testing.T) {
	for _, tc := range []struct {
		name       string
		workflowID string
		policy     temporalEnums.WorkflowIdConflictPolicy
		getErr     error
		startErr   error
		wantCode   int
	}{
		{name: "forwards the caller ID, running conflict policy, and closed execution reuse policy", workflowID: "core-grpc-DeleteSku-site-sku", policy: temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_FAIL},
		{name: "rejects an empty workflow ID"},
		{name: "missing Temporal history is not a missing Core SKU", workflowID: "core-grpc-DeleteSku-site-sku", getErr: serviceerror.NewNotFound("history unavailable"), wantCode: http.StatusServiceUnavailable},
		{name: "missing Temporal namespace is not a missing Core SKU", workflowID: "core-grpc-DeleteSku-site-sku", getErr: serviceerror.NewNamespaceNotFound("namespace unavailable"), wantCode: http.StatusServiceUnavailable},
		{name: "namespace missing during workflow start", workflowID: "core-grpc-DeleteSku-site-sku", startErr: serviceerror.NewNamespaceNotFound("namespace unavailable"), wantCode: http.StatusServiceUnavailable},
		{name: "raw gRPC NotFound is not a typed Core reply", workflowID: "core-grpc-DeleteSku-site-sku", getErr: status.Error(codes.NotFound, "missing"), wantCode: http.StatusServiceUnavailable},
		{name: "typed Core NotFound preserves cleanup status", workflowID: "core-grpc-DeleteSku-site-sku", getErr: tp.NewApplicationError("SKU missing", swe.ErrTypeNICoObjectNotFound), wantCode: http.StatusNotFound},
		{name: "legacy typed Core NotFound preserves cleanup status", workflowID: "core-grpc-DeleteSku-site-sku", getErr: tp.NewApplicationError("SKU missing", swe.ErrTypeCarbideObjectNotFound), wantCode: http.StatusNotFound},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client, started := newProxyClient(tc.getErr, tc.startErr)
			apiErr := ExecuteCoreGRPCWithConflictPolicy(context.Background(), client,
				corev1.Forge_DeleteSku_FullMethodName, &corev1.SkuIdList{Ids: []string{"sku"}}, nil,
				tc.workflowID, tc.policy, "")
			if tc.workflowID == "" {
				require.NotNil(t, apiErr)
				assert.Equal(t, http.StatusInternalServerError, apiErr.Code)
				client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
				return
			}
			if tc.wantCode == 0 {
				require.Nil(t, apiErr)
			} else {
				require.NotNil(t, apiErr)
				assert.Equal(t, tc.wantCode, apiErr.Code)
				cause := tc.getErr
				if tc.startErr != nil {
					cause = tc.startErr
				}
				assert.ErrorIs(t, apiErr, cause)
			}
			assert.Equal(t, tc.workflowID, started.options.ID)
			assert.Equal(t, grpcproxy.Core.WorkflowName, started.workflowName)
			assert.Equal(t, tc.policy, started.options.WorkflowIDConflictPolicy)
			assert.Equal(t, temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE, started.options.WorkflowIDReusePolicy)
			assert.False(t, started.options.WorkflowExecutionErrorWhenAlreadyStarted)
			assertNoTermination(t, client)
		})
	}
}

func TestExecuteCoreGRPCWithWorkflowID(t *testing.T) {
	activityFailure := func(activityName string, cause error) error {
		converter := tp.GetDefaultFailureConverter()
		return converter.FailureToError(&temporalFailure.Failure{
			Message: "activity failed",
			Cause:   converter.ErrorToFailure(cause),
			FailureInfo: &temporalFailure.Failure_ActivityFailureInfo{
				ActivityFailureInfo: &temporalFailure.ActivityFailureInfo{
					ActivityType: &temporalCommon.ActivityType{Name: activityName},
				},
			},
		})
	}
	coreFailure := func(code codes.Code) error {
		return activityFailure("InvokeCoreGRPCOnSite", swe.WrapErr(status.Error(code, "Core reply")))
	}
	cases := []struct {
		name             string
		startNewAttempt  bool
		startErr         error
		lookupRunID      *string
		getErr           error
		responseJSON     string
		cancelCaller     bool
		emptyWorkflowID  bool
		expectedCode     int
		expectedObserved bool
		expectedAllDone  bool
	}{
		{
			name: "new attempt returns completed cleanup", startNewAttempt: true,
			responseJSON: `{"allDone":true}`, expectedObserved: true, expectedAllDone: true,
		},
		{
			name: "new attempt returns incomplete cleanup", startNewAttempt: true,
			responseJSON: `{}`, expectedObserved: true,
		},
		{
			name: "duplicate running attempt joins its execution", startNewAttempt: true,
			startErr:    serviceerror.NewWorkflowExecutionAlreadyStarted("running", "request-1", "running-run"),
			lookupRunID: cutil.GetPtr("running-run"), responseJSON: `{}`, expectedObserved: true,
		},
		{
			name: "duplicate completed attempt reads its result", startNewAttempt: true,
			startErr:    serviceerror.NewWorkflowExecutionAlreadyStarted("completed", "request-1", "completed-run"),
			lookupRunID: cutil.GetPtr("completed-run"), responseJSON: `{"allDone":true}`,
			expectedObserved: true, expectedAllDone: true,
		},
		{
			name:        "persisted attempt retrieves completed result without starting",
			lookupRunID: cutil.GetPtr(""), responseJSON: `{"allDone":true}`,
			expectedObserved: true, expectedAllDone: true,
		},
		{
			name:        "expired workflow history never restarts a persisted attempt",
			lookupRunID: cutil.GetPtr(""), getErr: serviceerror.NewNotFound("workflow history expired"),
			expectedCode: http.StatusServiceUnavailable,
		},
		{
			name: "missing namespace while starting is not Core NotFound", startNewAttempt: true,
			startErr: serviceerror.NewNamespaceNotFound("missing-site"), expectedCode: http.StatusInternalServerError,
		},
		{
			name: "start deadline preserves unknown execution", startNewAttempt: true,
			startErr: serviceerror.NewDeadlineExceeded("start acknowledgement lost"), expectedCode: http.StatusGatewayTimeout,
		},
		{
			name:        "caller timeout while recovering leaves execution alone",
			lookupRunID: cutil.GetPtr(""), getErr: context.Canceled, cancelCaller: true,
			expectedCode: http.StatusGatewayTimeout,
		},
		{
			name:        "activity timeout is not a Core completion result",
			lookupRunID: cutil.GetPtr(""), getErr: tp.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil),
			expectedCode: http.StatusGatewayTimeout,
		},
		{
			name:        "Core NotFound is a definitive reply",
			lookupRunID: cutil.GetPtr(""), getErr: coreFailure(codes.NotFound),
			expectedCode: http.StatusNotFound, expectedObserved: true,
		},
		{
			name:         "legacy Core NotFound is a definitive reply",
			lookupRunID:  cutil.GetPtr(""),
			getErr:       activityFailure("InvokeCoreGRPCOnSite", tp.NewNonRetryableApplicationError("absent", swe.ErrTypeCarbideObjectNotFound, nil)),
			expectedCode: http.StatusNotFound, expectedObserved: true,
		},
		{
			name:        "Core precondition rejection is observed but not success",
			lookupRunID: cutil.GetPtr(""), getErr: coreFailure(codes.FailedPrecondition),
			expectedCode: http.StatusPreconditionFailed, expectedObserved: true,
		},
		{
			name:        "Core transport unavailability remains ambiguous",
			lookupRunID: cutil.GetPtr(""), getErr: coreFailure(codes.Unavailable),
			expectedCode: http.StatusServiceUnavailable,
		},
		{
			name:        "client response size exhaustion remains ambiguous",
			lookupRunID: cutil.GetPtr(""), getErr: coreFailure(codes.ResourceExhausted),
			expectedCode: http.StatusTooManyRequests,
		},
		{
			name:         "unrecognized activity failure is not a definitive Core reply",
			lookupRunID:  cutil.GetPtr(""),
			getErr:       activityFailure("InvokeCoreGRPCOnSite", tp.NewApplicationError("connection failed", "unknown")),
			expectedCode: http.StatusInternalServerError,
		},
		{
			name:         "NotFound from another activity is not Core completion",
			lookupRunID:  cutil.GetPtr(""),
			getErr:       activityFailure("OtherActivity", swe.WrapErr(status.Error(codes.NotFound, "absent"))),
			expectedCode: http.StatusServiceUnavailable,
		},
		{
			name:        "unattributed gRPC NotFound is not Core completion",
			lookupRunID: cutil.GetPtr(""), getErr: status.Error(codes.NotFound, "workflow lookup failed"),
			expectedCode: http.StatusServiceUnavailable,
		},
		{
			name: "absent result payload remains unknown", lookupRunID: cutil.GetPtr(""),
			expectedCode: http.StatusInternalServerError,
		},
		{
			name: "malformed JSON result remains unknown", lookupRunID: cutil.GetPtr(""), responseJSON: `{`,
			expectedCode: http.StatusInternalServerError,
		},
		{
			name: "malformed typed result remains unknown", lookupRunID: cutil.GetPtr(""), responseJSON: `{"allDone":"yes"}`,
			expectedCode: http.StatusInternalServerError,
		},
		{
			name: "missing attempt ID never dispatches", emptyWorkflowID: true, startNewAttempt: true,
			expectedCode: http.StatusInternalServerError,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			temporalClient := &tmocks.Client{}
			workflowRun := &tmocks.WorkflowRun{}
			workflowID := "force-delete-attempt-1"
			if tc.emptyWorkflowID {
				workflowID = ""
			} else {
				if tc.startNewAttempt {
					temporalClient.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(options tclient.StartWorkflowOptions) bool {
						assert.Equal(t, workflowID, options.ID)
						assert.Equal(t, temporalEnums.WORKFLOW_ID_REUSE_POLICY_REJECT_DUPLICATE, options.WorkflowIDReusePolicy)
						assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_FAIL, options.WorkflowIDConflictPolicy)
						assert.True(t, options.WorkflowExecutionErrorWhenAlreadyStarted)
						assert.Equal(t, grpcproxy.WorkflowExecutionTimeout, options.WorkflowExecutionTimeout)
						return true
					}), grpcproxy.Core.WorkflowName, mock.MatchedBy(func(req grpcproxy.Request) bool {
						assert.Equal(t, corev1.Forge_AdminForceDeleteMachine_FullMethodName, req.FullMethod)
						assert.JSONEq(t, `{"hostQuery":"machine-1"}`, string(req.RequestJSON))
						return true
					})).Return(workflowRun, tc.startErr).Once()
				}
				if tc.lookupRunID != nil {
					temporalClient.On("GetWorkflow", mock.Anything, workflowID, *tc.lookupRunID).Return(workflowRun).Once()
				}
				if tc.startErr == nil || tc.lookupRunID != nil {
					workflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
						args.Get(1).(*grpcproxy.Response).ResponseJSON = []byte(tc.responseJSON)
					}).Return(tc.getErr).Once()
				}
			}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if tc.cancelCaller {
				cancel()
			}
			response := &corev1.AdminForceDeleteMachineResponse{}
			apiErr, observed := ExecuteCoreGRPCWithWorkflowID(ctx, temporalClient,
				corev1.Forge_AdminForceDeleteMachine_FullMethodName,
				&corev1.AdminForceDeleteMachineRequest{HostQuery: "machine-1"}, response,
				workflowID, tc.startNewAttempt, "")

			if tc.expectedCode == 0 {
				require.Nil(t, apiErr)
			} else {
				require.NotNil(t, apiErr)
				assert.Equal(t, tc.expectedCode, apiErr.Code)
			}
			assert.Equal(t, tc.expectedObserved, observed)
			assert.Equal(t, tc.expectedAllDone, response.AllDone)
			assertNoTermination(t, temporalClient)
			if !tc.startNewAttempt || tc.emptyWorkflowID {
				temporalClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 0)
			}
			temporalClient.AssertExpectations(t)
			workflowRun.AssertExpectations(t)
		})
	}
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
