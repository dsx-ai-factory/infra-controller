// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	commonpb "go.temporal.io/api/common/v1"
	enumspb "go.temporal.io/api/enums/v1"
	failurepb "go.temporal.io/api/failure/v1"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestSkuComponentReplacementRejected(t *testing.T) {
	// The shared resolver uses the same sentinel as v2.2.2. The Core-only
	// resolver is from v2.0; keep its historical text. Serialize their native
	// Go types just as the Agent does.
	unknownMethod := fmt.Errorf("%w: %q on %q", client.ErrUnknownProxyMethod, "ReplaceSkuComponents", "forge.Forge")
	unknownCoreMethod := fmt.Errorf("%w: %q", errors.New("unknown NICo Core method"), "ReplaceSkuComponents")
	unimplemented := status.Error(codes.Unimplemented, "")
	forbidden := status.Error(codes.PermissionDenied, "unexpected HTTP status code received from server: 403 (Forbidden); malformed header: missing HTTP content-type")

	// The workflow client adds its own wrapper outside the activity failure.
	// Use the test engine to produce that wrapper rather than flattening it.
	var suite testsuite.WorkflowTestSuite
	env := suite.NewTestWorkflowEnvironment()
	env.ExecuteWorkflow(func(workflow.Context) error {
		return skuProxyActivityError("InvokeCoreGRPCOnSite", unknownMethod)
	})
	workflowFailure := env.GetWorkflowError()
	var workflowErr *temporal.WorkflowExecutionError
	require.ErrorAs(t, workflowFailure, &workflowErr)

	cases := []struct {
		name     string
		err      error
		rejected bool
	}{
		{
			name:     "older shared Agent has no method descriptor",
			err:      cutil.NewAPIError(http.StatusInternalServerError, unknownMethod.Error(), nil).WithCause(workflowFailure),
			rejected: true,
		},
		{
			name:     "older Core-only Agent has no method descriptor",
			err:      skuProxyActivityError("InvokeCoreGRPCOnSite", unknownCoreMethod),
			rejected: true,
		},
		{
			name:     "newer Agent adds a typed descriptor rejection",
			err:      skuProxyActivityError("InvokeCoreGRPCOnSite", temporal.NewNonRetryableApplicationError(unknownMethod.Error(), swe.ErrTypeNICoUnimplemented, unknownMethod)),
			rejected: true,
		},
		{
			name:     "Core dispatcher does not recognize the RPC",
			err:      skuProxyActivityError("InvokeCoreGRPCOnSite", swe.WrapErr(unimplemented)),
			rejected: true,
		},
		{
			name:     "older Agent wraps the Core dispatcher rejection",
			err:      skuProxyActivityError("InvokeCoreGRPCOnSite", temporal.NewNonRetryableApplicationError(unimplemented.Error(), swe.ErrTypeCarbideUnimplemented, unimplemented)),
			rejected: true,
		},
		{
			name:     "Core auth rejects the RPC before dispatch",
			err:      skuProxyActivityError("InvokeCoreGRPCOnSite", swe.WrapErr(forbidden)),
			rejected: true,
		},
		{
			name:     "older Agent wraps the Core auth rejection",
			err:      skuProxyActivityError("InvokeCoreGRPCOnSite", temporal.NewNonRetryableApplicationError(forbidden.Error(), swe.ErrTypeCarbideDenied, forbidden)),
			rejected: true,
		},
		{
			name: "same error outside the proxy activity",
			err:  unknownMethod,
		},
		{
			name: "same error from another activity",
			err:  skuProxyActivityError("InvokeFlowGRPCOnSite", unknownMethod),
		},
		{
			name: "matching text without the descriptor sentinel",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", errors.New(unknownMethod.Error())),
		},
		{
			name: "descriptor rejection for another method",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", fmt.Errorf("%w: %q on %q", errors.New("unknown proxied gRPC method"), "ReplaceSku", "forge.Forge")),
		},
		{
			name: "descriptor message with extra context",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", fmt.Errorf("%w after replacement", unknownMethod)),
		},
		{
			name: "RPC handler reports unsupported behavior",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", swe.WrapErr(status.Error(codes.Unimplemented, "component replacement is disabled"))),
		},
		{
			name: "RPC handler reports permission denied",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", swe.WrapErr(status.Error(codes.PermissionDenied, "SKU update is not permitted"))),
		},
		{
			name: "response decoding failure",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", fmt.Errorf("decode response for ReplaceSkuComponents: %w", forbidden)),
		},
		{
			name: "Core becomes unavailable",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", swe.WrapErr(status.Error(codes.Unavailable, "connection closed"))),
		},
		{
			name: "RPC deadline expires",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", context.DeadlineExceeded),
		},
		{
			name: "activity times out with an earlier rejection as its cause",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", temporal.NewTimeoutError(enumspb.TIMEOUT_TYPE_START_TO_CLOSE, unknownMethod)),
		},
		{
			name: "workflow timeout contains a rejected activity",
			err:  temporal.NewTimeoutError(enumspb.TIMEOUT_TYPE_START_TO_CLOSE, skuProxyActivityError("InvokeCoreGRPCOnSite", unknownMethod)),
		},
		{
			name: "empty unimplemented without the Agent wrapper",
			err:  skuProxyActivityError("InvokeCoreGRPCOnSite", unimplemented),
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.rejected, skuComponentReplacementRejected(tc.err))
		})
	}
}

// skuProxyActivityError reproduces Temporal's activity failure envelope around
// the Agent's serialized error, including conversion of Go errors to
// ApplicationErrors. The classifier receives the typed causes that REST sees.
func skuProxyActivityError(activityName string, cause error) error {
	converter := temporal.NewDefaultFailureConverter(temporal.DefaultFailureConverterOptions{})
	failure := &failurepb.Failure{
		Message: "Activity task failed",
		Cause:   converter.ErrorToFailure(cause),
		FailureInfo: &failurepb.Failure_ActivityFailureInfo{
			ActivityFailureInfo: &failurepb.ActivityFailureInfo{
				ActivityType: &commonpb.ActivityType{Name: activityName},
				RetryState:   enumspb.RETRY_STATE_MAXIMUM_ATTEMPTS_REACHED,
			},
		},
	}
	return converter.FailureToError(failure)
}
