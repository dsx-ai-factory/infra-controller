// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"path"
	"strings"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tclient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
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
	return executeGRPCProxy(ctx, stc, grpcproxy.Core, fullMethod, req, resp, workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, secretKey, secretFields...)
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
// TerminateWorkflowOnTimeOut; see executeGRPCProxy for why the proxy leaves the
// execution alone.
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
	return executeGRPCProxy(ctx, stc, grpcproxy.Flow, fullMethod, req, resp, workflowID, conflictPolicy, secretKey, secretFields...)
}

// ProxyFlowGRPC dispatches one already-validated request to Flow through the
// generic proxy workflow, decoding the reply into resp, which may be nil for
// methods with an empty response.
//
// Callers pass a deterministic workflow ID with USE_EXISTING where concurrent
// identical requests should coalesce onto one in-flight Flow call, and a fresh
// ID with UNSPECIFIED where they must not.
//
// It returns nil on success and otherwise a rendered Echo response, so handlers
// report failures without replacing Flow's status code and message with a
// generic wrapper. The internal cause stays in the log: an error in the
// response body serializes to an empty object, which tells a client nothing
// and contradicts the null the schema promises.
func ProxyFlowGRPC(
	ctx context.Context,
	c echo.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
) error {
	return proxyFlowGRPC(
		ctx, c, logger, stc, fullMethod, req, resp, workflowID, conflictPolicy, "",
	)
}

// ProxyFlowGRPCWithSecrets behaves like ProxyFlowGRPC while redacting the named
// top-level protojson fields from Temporal-visible request JSON and carrying
// their original values encrypted with secretKey.
func ProxyFlowGRPCWithSecrets(
	ctx context.Context,
	c echo.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	secretKey string,
	secretFields ...string,
) error {
	return proxyFlowGRPC(
		ctx, c, logger, stc, fullMethod, req, resp, workflowID, conflictPolicy,
		secretKey, secretFields...,
	)
}

func proxyFlowGRPC(
	ctx context.Context,
	c echo.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	secretKey string,
	secretFields ...string,
) error {
	apiErr := ExecuteFlowGRPC(
		ctx, stc, fullMethod, req, resp, workflowID, conflictPolicy,
		secretKey, secretFields...,
	)
	if apiErr == nil {
		return nil
	}

	logger.Error().Err(apiErr.Diagnosis()).Str("Method", path.Base(fullMethod)).Msg("failed to proxy request to Flow")
	return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
}

// proxyTimedOutError reports that no result arrived in time. The client-facing
// message is the same however the request ran out of time, because the client
// learns the same thing either way; detail is what carries the distinction into
// the log, and it matters because only an execution timeout proves the
// execution has stopped.
func proxyTimedOutError(backend grpcproxy.Backend, fullMethod, detail string, cause error) *cutil.APIError {
	return cutil.NewAPIError(http.StatusGatewayTimeout, fmt.Sprintf("%s proxy request timed out", backend.Label), fmt.Errorf("%s proxy %s %s: %w", strings.ToLower(backend.Label), fullMethod, detail, cause))
}

// executeGRPCProxy starts one proxy workflow and translates its result. Both
// backends share it; they differ only in the backend descriptor and in who
// chooses the workflow ID and conflict policy.
//
// It never terminates the execution, including when the caller stops waiting.
// The activity does not heartbeat, so Temporal cannot deliver cancellation while
// its backend RPC is in progress, and terminating only the workflow would
// discard the result without stopping that RPC. For the Flow callers that pass a
// deterministic ID with USE_EXISTING it would also free that ID, so a retried
// request would start a duplicate mutation instead of attaching to the call
// already in flight. An abandoned execution stays bounded by
// grpcproxy.WorkflowExecutionTimeout and grpcproxy.ActivityStartToCloseTimeout.
func executeGRPCProxy(
	ctx context.Context,
	stc tclient.Client,
	backend grpcproxy.Backend,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	secretKey string,
	secretFields ...string,
) *cutil.APIError {
	// Refuse the combination rather than fall through to sending the request
	// unredacted: without a key the secrets cannot travel encrypted, and the
	// caller named them precisely because they must not reach Temporal history
	// in cleartext.
	if len(secretFields) > 0 && secretKey == "" {
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to encode %s proxy request", backend.Label), fmt.Errorf("secret fields named for %s without a secret key", fullMethod))
	}

	reqJSON, err := protojson.Marshal(req)
	if err != nil {
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to encode %s proxy request", backend.Label), fmt.Errorf("encode request for %s: %w", fullMethod, err))
	}

	// Redact any secret fields from the Temporal-visible request and carry them
	// AES-encrypted so they never appear in Temporal history in cleartext. The
	// site decrypts with the same key (the site ID) and merges them back.
	var encryptedSecrets []byte
	if len(secretFields) > 0 {
		redacted, secretsJSON, rerr := grpcproxy.RedactSecrets(reqJSON, secretFields)
		if rerr != nil {
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to redact %s proxy request", backend.Label), rerr)
		}
		reqJSON = redacted
		if len(secretsJSON) > 0 {
			encryptedSecrets = cutil.EncryptData(secretsJSON, secretKey)
		}
	}

	workflowOptions := tclient.StartWorkflowOptions{
		ID:                       workflowID,
		WorkflowExecutionTimeout: grpcproxy.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDReusePolicy:    temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
		WorkflowIDConflictPolicy: conflictPolicy,
	}

	wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	we, err := stc.ExecuteWorkflow(wfCtx, workflowOptions, backend.WorkflowName, grpcproxy.Request{
		FullMethod:       fullMethod,
		RequestJSON:      reqJSON,
		EncryptedSecrets: encryptedSecrets,
	})
	if err != nil {
		// Either way the start may have reached the server before the wait
		// ended, so these are timeouts rather than failures to dispatch. The
		// second is not the caller giving up: the SDK caps a start at ten
		// seconds regardless of how long the caller is prepared to wait, and
		// never retries that deadline, so a slow frontend surfaces here with
		// the caller still waiting.
		if wfCtx.Err() != nil {
			return proxyTimedOutError(backend, fullMethod, "caller stopped waiting", wfCtx.Err())
		}
		var startDeadlineErr *serviceerror.DeadlineExceeded
		if errors.As(err, &startDeadlineErr) {
			return proxyTimedOutError(backend, fullMethod, "start deadline exceeded", err)
		}
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute %s proxy workflow", backend.Label), fmt.Errorf("execute %s workflow: %w", backend.WorkflowName, err))
	}

	var out grpcproxy.Response
	resultErr := we.Get(wfCtx, &out)
	if resultErr != nil {
		// A Temporal timeout is positive evidence that the execution closed, so
		// it is checked first: when the caller also went away, having closed is
		// the more useful fact of the two.
		var timeoutErr *tp.TimeoutError
		if errors.As(resultErr, &timeoutErr) {
			return proxyTimedOutError(backend, fullMethod, "execution timed out", resultErr)
		}

		// Only wfCtx proves the caller stopped waiting. A DeadlineExceeded in
		// resultErr alone does not: it can come from inside the execution, and
		// treating it as the caller's would misreport who gave up. Unlike the
		// start, the wait for a result runs to wfCtx's deadline, so there is no
		// shorter SDK deadline to distinguish from it here.
		if wfCtx.Err() != nil {
			return proxyTimedOutError(backend, fullMethod, "caller stopped waiting", wfCtx.Err())
		}

		code, werr := UnwrapWorkflowError(resultErr)
		return cutil.NewAPIError(code, werr.Error(), nil)
	}

	if resp != nil && len(out.ResponseJSON) > 0 {
		decodeErr := (protojson.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(out.ResponseJSON, resp)
		if decodeErr != nil {
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to decode %s proxy response", backend.Label), fmt.Errorf("decode response for %s: %w", fullMethod, decodeErr))
		}
	}
	return nil
}
