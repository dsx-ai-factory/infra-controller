// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// testFlowProxyReply makes run resolve to msg the way the site's Flow proxy
// activity does, as protojson inside a grpcproxy.Response rather than as the
// bare response proto.
func testFlowProxyReply(t *testing.T, run *tmocks.WorkflowRun, msg proto.Message) {
	t.Helper()

	responseJSON, err := protojson.Marshal(msg)
	require.NoError(t, err)

	run.Mock.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		args.Get(1).(*grpcproxy.Response).ResponseJSON = responseJSON
	}).Return(nil)
}

func testFlowProxyFailure(run *tmocks.WorkflowRun, resultErr error) {
	run.Mock.On("Get", mock.Anything, mock.Anything).Return(resultErr)
}

func assertSingleFlowProxyErrorResponse(
	t *testing.T,
	ec echo.Context,
	recorder *httptest.ResponseRecorder,
	handlerErr error,
	resultErr error,
) {
	t.Helper()

	require.NoError(t, handlerErr)
	assert.Equal(t, recorder.Code, ec.Response().Status)
	var response map[string]any
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response), "response must contain exactly one JSON value")
	assert.Equal(t, resultErr.Error(), response["message"])
	assert.Contains(t, response, "data")
	assert.Nil(t, response["data"])
	assert.NotContains(t, response, "taskIds")
}

// testFlowProxyDispatch mocks the Flow proxy workflow start, asserting that the
// handler asked for fullMethod, and returns a pointer to the options it was
// started with so a test can also assert the derived workflow ID and conflict
// policy. Checking the method here is what catches an endpoint wired to the
// wrong Flow call, which the workflow ID alone cannot show.
func testFlowProxyDispatch(t *testing.T, mockTC *tmocks.Client, run *tmocks.WorkflowRun, fullMethod string, execErr error) *tClient.StartWorkflowOptions {
	t.Helper()

	started := &tClient.StartWorkflowOptions{}
	mockTC.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Flow.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			*started = args.Get(1).(tClient.StartWorkflowOptions)
			assert.Equal(t, fullMethod, args.Get(3).(grpcproxy.Request).FullMethod)
		}).
		Return(run, execErr)
	return started
}

// testFlowProxyMethodDispatch mocks the Flow proxy workflow start for one Flow
// method, routing on the method instead of asserting it. A handler that makes
// several Flow calls needs each to resolve to its own reply, which
// testFlowProxyDispatch cannot do because it matches any method. inspect, when
// non-nil, runs on the dispatched arguments so a test can decode the request.
func testFlowProxyMethodDispatch(t *testing.T, mockTC *tmocks.Client, run *tmocks.WorkflowRun, fullMethod string, inspect func(mock.Arguments)) *tClient.StartWorkflowOptions {
	t.Helper()

	started := &tClient.StartWorkflowOptions{}
	mockTC.Mock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Flow.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool { return req.FullMethod == fullMethod })).
		Run(func(args mock.Arguments) {
			*started = args.Get(1).(tClient.StartWorkflowOptions)
			if inspect != nil {
				inspect(args)
			}
		}).
		Return(run, nil)
	return started
}

// testFlowProxyRequest decodes the Flow request the handler sent through the
// proxy into req, so tests can assert on the fields the transport carries as
// protojson rather than as a typed proto argument.
func testFlowProxyRequest(t *testing.T, args mock.Arguments, req proto.Message) {
	t.Helper()

	proxyReq, ok := args.Get(3).(grpcproxy.Request)
	require.True(t, ok, "workflow arg must be a grpcproxy.Request")
	require.NoError(t, protojson.Unmarshal(proxyReq.RequestJSON, req))
}

// testFlowProxyRequestWithSecrets restores the encrypted fields the same way
// the site activity does, then decodes the complete Flow request into req.
func testFlowProxyRequestWithSecrets(t *testing.T, args mock.Arguments, secretKey, expectedSecret string, req proto.Message) {
	t.Helper()

	proxyReq, ok := args.Get(3).(grpcproxy.Request)
	require.True(t, ok, "workflow arg must be a grpcproxy.Request")
	require.NotEmpty(t, proxyReq.EncryptedSecrets)
	require.NotContains(t, string(proxyReq.RequestJSON), expectedSecret,
		"authentication data must not be present in Temporal request JSON")
	var readableRequest map[string]any
	require.NoError(t, json.Unmarshal(proxyReq.RequestJSON, &readableRequest))
	require.Equal(t, grpcproxy.RedactedPlaceholder, readableRequest["authenticationData"])
	secretsJSON := cutil.DecryptData(proxyReq.EncryptedSecrets, secretKey)
	restoredJSON, err := grpcproxy.MergeSecrets(proxyReq.RequestJSON, secretsJSON)
	require.NoError(t, err)
	require.NoError(t, protojson.Unmarshal(restoredJSON, req))
}
