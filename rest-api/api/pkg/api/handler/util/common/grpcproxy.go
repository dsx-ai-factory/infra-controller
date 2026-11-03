// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"path"

	"github.com/rs/zerolog"
	temporalEnums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/siteproxy"
)

// ExecuteCoreGRPC proxies one already-validated NICo Core (forge.Forge) gRPC
// request via the generic site proxy workflow. See siteproxy.ExecuteCoreGRPC,
// which owns the transport so cloud workflow activities can share it without
// depending on the REST handler tree.
func ExecuteCoreGRPC(
	ctx context.Context,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	secretKey string,
	secretFields ...string,
) *cutil.APIError {
	return siteproxy.ExecuteCoreGRPC(ctx, stc, fullMethod, req, resp, secretKey, secretFields...)
}

// ExecuteFlowGRPC proxies one already-validated Flow (v1.Flow) gRPC request via
// the generic site proxy workflow with a caller-chosen workflow ID and conflict
// policy. See siteproxy.ExecuteFlowGRPC.
//
// On timeout it returns StatusGatewayTimeout. Do not follow that with
// TerminateWorkflowOnTimeOut; the proxy deliberately leaves the execution alone.
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
	return siteproxy.ExecuteFlowGRPC(ctx, stc, fullMethod, req, resp, workflowID, conflictPolicy, secretKey, secretFields...)
}

// ProxyFlowGRPC dispatches one already-validated request to Flow through the
// generic proxy workflow, decoding the reply into resp, which may be nil for
// methods with an empty response.
//
// Callers pass a deterministic workflow ID with USE_EXISTING where concurrent
// identical requests should coalesce onto one in-flight Flow call, and a fresh
// ID with UNSPECIFIED where they must not.
//
// It returns nil when Flow succeeds and an APIError when Flow fails. The HTTP
// handler owns rendering that error so this transport helper cannot write a
// response and then allow the handler to continue down its success path.
//
// The internal cause stays in the log: an error in the response body
// serializes to an empty object, which tells a client nothing and contradicts
// the null the schema promises.
func ProxyFlowGRPC(
	ctx context.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
) *cutil.APIError {
	return proxyFlowGRPC(
		ctx, logger, stc, fullMethod, req, resp, workflowID, conflictPolicy, "",
	)
}

// ProxyFlowGRPCWithSecrets behaves like ProxyFlowGRPC while redacting the named
// top-level protojson fields from Temporal-visible request JSON and carrying
// their original values encrypted with secretKey.
func ProxyFlowGRPCWithSecrets(
	ctx context.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	secretKey string,
	secretFields ...string,
) *cutil.APIError {
	return proxyFlowGRPC(
		ctx, logger, stc, fullMethod, req, resp, workflowID, conflictPolicy,
		secretKey, secretFields...,
	)
}

func proxyFlowGRPC(
	ctx context.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	secretKey string,
	secretFields ...string,
) *cutil.APIError {
	apiErr := ExecuteFlowGRPC(
		ctx, stc, fullMethod, req, resp, workflowID, conflictPolicy,
		secretKey, secretFields...,
	)
	if apiErr == nil {
		return nil
	}

	logger.Error().Err(apiErr.Diagnosis()).Str("Method", path.Base(fullMethod)).Msg("failed to proxy request to Flow")
	return apiErr
}
