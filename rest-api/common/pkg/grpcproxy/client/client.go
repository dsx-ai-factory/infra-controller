// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package client provides the Site gRPC transport shared by REST handlers and workflow activities.
package client

import (
	"context"
	"fmt"
	"net/http"
	"path"
	"strings"

	"github.com/google/uuid"
	temporalEnums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ExecuteCoreGRPC proxies one already-validated NICo Core (forge.Forge) gRPC
// request via the generic site proxy workflow. A REST handler may call this
// helper zero, one, or many times depending on how many Core invocations it
// needs. The caller supplies the typed request proto; it is protojson-encoded
// for transport so it is readable in the Temporal UI, and the protojson
// response is decoded into resp (which may be nil for methods with an empty
// response).
//
// Every request gets a fresh workflow ID, so concurrent identical Core calls
// never coalesce.
//
// Naming secretFields requires a non-empty secretKey; the site has no other way
// to recover the redacted values.
//
// It returns an APIError when the proxy request fails so handlers can surface
// the status code and message without replacing Core/Temporal details with a
// generic wrapper.
func ExecuteCoreGRPC(
	ctx context.Context,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	secretKey string,
	secretFields ...string,
) *cutil.APIError {
	workflowID := fmt.Sprintf("core-grpc-%s-%s", path.Base(fullMethod), uuid.NewString())
	invocation, apiErr := newProxyInvocation(stc, grpcproxy.Core, fullMethod, req, secretKey, secretFields...)
	if apiErr != nil {
		return apiErr
	}
	return invocation.execute(ctx, resp, workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED)
}

// ExecuteCoreGRPCWithWorkflowID starts or recovers one durably recorded Core
// attempt. Only the caller that reserved a new attempt may set startNewAttempt.
// Recovery retrieves the existing result without starting a workflow, including
// when its history is missing or expired. A duplicate start joins the recorded
// execution instead of repeating the Core call.
//
// The second result, coreResultObserved, is true for a decoded response or a
// definitive typed Core reply. It does not mean the requested operation succeeded:
// callers must still inspect the API error and response. Missing history, malformed
// results, timeouts, and transport-ambiguous errors leave it false. Never infer
// completion from an HTTP status alone or terminate the recorded execution.
func ExecuteCoreGRPCWithWorkflowID(
	ctx context.Context,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	startNewAttempt bool,
	secretKey string,
	secretFields ...string,
) (*cutil.APIError, bool) {
	if strings.TrimSpace(workflowID) == "" {
		return cutil.NewAPIError(http.StatusInternalServerError, "Core proxy attempt requires a workflow ID", nil), false
	}
	invocation, apiErr := newProxyInvocation(stc, grpcproxy.Core, fullMethod, req, secretKey, secretFields...)
	if apiErr != nil {
		return apiErr, false
	}
	return invocation.executeAttempt(ctx, resp, workflowID, startNewAttempt)
}

// ExecuteFlowGRPC proxies one already-validated Flow (v1.Flow) gRPC request via
// the generic site proxy workflow. Unlike ExecuteCoreGRPC, the caller supplies
// the Temporal workflow ID and conflict policy so handlers can keep read dedup
// (deterministic IDs + USE_EXISTING) and create freshness (per-request UUIDs)
// without a bespoke workflow per method.
//
// Naming secretFields requires a non-empty secretKey; the site has no other way
// to recover the redacted values.
//
// On timeout it returns StatusGatewayTimeout. Do not follow that with
// TerminateWorkflowOnTimeOut: the backend RPC may still be running, and a retry
// must be able to join the existing execution.
func ExecuteFlowGRPC(
	ctx context.Context,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	secretKey string,
	secretFields ...string,
) *cutil.APIError {
	invocation, apiErr := newProxyInvocation(stc, grpcproxy.Flow, fullMethod, req, secretKey, secretFields...)
	if apiErr != nil {
		return apiErr
	}
	return invocation.execute(ctx, resp, workflowID, conflictPolicy)
}
