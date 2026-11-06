// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package siteproxy proxies one already-validated NICo Core or Flow gRPC
// request through the generic site proxy workflow and translates its result
// into an APIError.
//
// It lives in the workflow module tree, rather than in the REST handler
// utilities, so cloud workflow activities can share the transport with REST
// handlers. The workflow image is built
// without the api and auth trees, so this package must depend only on what that
// image already copies. The REST handler package re-exports these helpers.
package siteproxy

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"path"
	"strings"

	"github.com/google/uuid"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tclient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
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
	mode := proxyResumeAttempt
	if startNewAttempt {
		mode = proxyStartAttempt
	}
	return runGRPCProxy(ctx, stc, grpcproxy.Core, fullMethod, req, resp, workflowID,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_FAIL, mode, secretKey, secretFields...)
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
	apiErr, _ := runGRPCProxy(ctx, stc, backend, fullMethod, req, resp, workflowID,
		conflictPolicy, proxyDefaultWorkflow, secretKey, secretFields...)
	return apiErr
}

type proxyWorkflowMode uint8

const (
	proxyDefaultWorkflow proxyWorkflowMode = iota
	proxyStartAttempt
	proxyResumeAttempt
)

func runGRPCProxy(
	ctx context.Context,
	stc tclient.Client,
	backend grpcproxy.Backend,
	fullMethod string,
	req proto.Message,
	resp proto.Message,
	workflowID string,
	conflictPolicy temporalEnums.WorkflowIdConflictPolicy,
	mode proxyWorkflowMode,
	secretKey string,
	secretFields ...string,
) (*cutil.APIError, bool) {
	// Refuse the combination rather than fall through to sending the request
	// unredacted: without a key the secrets cannot travel encrypted, and the
	// caller named them precisely because they must not reach Temporal history
	// in cleartext.
	if len(secretFields) > 0 && secretKey == "" {
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to encode %s proxy request", backend.Label), fmt.Errorf("secret fields named for %s without a secret key", fullMethod)), false
	}

	reqJSON, err := protojson.Marshal(req)
	if err != nil {
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to encode %s proxy request", backend.Label), fmt.Errorf("encode request for %s: %w", fullMethod, err)), false
	}

	// Redact any secret fields from the Temporal-visible request and carry them
	// AES-encrypted so they never appear in Temporal history in cleartext. The
	// site decrypts with the same key (the site ID) and merges them back.
	var encryptedSecrets []byte
	if len(secretFields) > 0 {
		redacted, secretsJSON, rerr := grpcproxy.RedactSecrets(reqJSON, secretFields)
		if rerr != nil {
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to redact %s proxy request", backend.Label), rerr), false
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
	if mode == proxyStartAttempt {
		workflowOptions.WorkflowIDReusePolicy = temporalEnums.WORKFLOW_ID_REUSE_POLICY_REJECT_DUPLICATE
		workflowOptions.WorkflowExecutionErrorWhenAlreadyStarted = true
	}

	wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	var we tclient.WorkflowRun
	if mode == proxyResumeAttempt {
		we = stc.GetWorkflow(wfCtx, workflowID, "")
	} else {
		we, err = stc.ExecuteWorkflow(wfCtx, workflowOptions, backend.WorkflowName, grpcproxy.Request{
			FullMethod:       fullMethod,
			RequestJSON:      reqJSON,
			EncryptedSecrets: encryptedSecrets,
		})
		var alreadyStarted *serviceerror.WorkflowExecutionAlreadyStarted
		if mode == proxyStartAttempt && errors.As(err, &alreadyStarted) {
			we = stc.GetWorkflow(wfCtx, workflowID, alreadyStarted.RunId)
			err = nil
		}
	}
	if err != nil {
		// Either way the start may have reached the server before the wait
		// ended, so these are timeouts rather than failures to dispatch. The
		// second is not the caller giving up: the SDK caps a start at ten
		// seconds regardless of how long the caller is prepared to wait, and
		// never retries that deadline, so a slow frontend surfaces here with
		// the caller still waiting.
		if wfCtx.Err() != nil {
			return proxyTimedOutError(backend, fullMethod, "caller stopped waiting", wfCtx.Err()), false
		}
		var startDeadlineErr *serviceerror.DeadlineExceeded
		if errors.As(err, &startDeadlineErr) {
			return proxyTimedOutError(backend, fullMethod, "start deadline exceeded", err), false
		}
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute %s proxy workflow", backend.Label), fmt.Errorf("execute %s workflow: %w", backend.WorkflowName, err)), false
	}

	var out grpcproxy.Response
	resultErr := we.Get(wfCtx, &out)
	if resultErr != nil {
		// A Temporal timeout is positive evidence that the execution closed, so
		// it is checked first: when the caller also went away, having closed is
		// the more useful fact of the two.
		var timeoutErr *tp.TimeoutError
		if errors.As(resultErr, &timeoutErr) {
			return proxyTimedOutError(backend, fullMethod, "execution timed out", resultErr), false
		}

		// Only wfCtx proves the caller stopped waiting. A DeadlineExceeded in
		// resultErr alone does not: it can come from inside the execution, and
		// treating it as the caller's would misreport who gave up. Unlike the
		// start, the wait for a result runs to wfCtx's deadline, so there is no
		// shorter SDK deadline to distinguish from it here.
		if wfCtx.Err() != nil {
			return proxyTimedOutError(backend, fullMethod, "caller stopped waiting", wfCtx.Err()), false
		}

		code, werr := UnwrapWorkflowError(resultErr)
		coreResultObserved := false
		if mode != proxyDefaultWorkflow {
			var activityErr *tp.ActivityError
			var applicationErr *tp.ApplicationError
			if errors.As(resultErr, &activityErr) && activityErr.ActivityType().GetName() == "InvokeCoreGRPCOnSite" && errors.As(resultErr, &applicationErr) {
				switch applicationErr.Type() {
				case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound,
					swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented,
					swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied,
					swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists,
					swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition,
					swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
					coreResultObserved = true
				}
			}
			var missingWorkflow *serviceerror.NotFound
			var missingNamespace *serviceerror.NamespaceNotFound
			if !coreResultObserved && (code == http.StatusNotFound || errors.As(resultErr, &missingWorkflow) || errors.As(resultErr, &missingNamespace)) {
				// A missing workflow or namespace is not a missing Core resource.
				code = http.StatusServiceUnavailable
			}
		}
		return cutil.NewAPIError(code, werr.Error(), nil).WithCause(resultErr), coreResultObserved
	}

	if mode != proxyDefaultWorkflow && !json.Valid(out.ResponseJSON) {
		return cutil.NewAPIError(http.StatusInternalServerError, "Core proxy result is missing or malformed", nil), false
	}
	if resp != nil && len(out.ResponseJSON) > 0 {
		decodeErr := (protojson.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(out.ResponseJSON, resp)
		if decodeErr != nil {
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to decode %s proxy response", backend.Label), fmt.Errorf("decode response for %s: %w", fullMethod, decodeErr)), false
		}
	}
	return nil, true
}

// UnwrapWorkflowError removes Temporal wrappers and maps backend errors to HTTP status codes.
func UnwrapWorkflowError(err error) (code int, unwrappedError error) {
	code = http.StatusInternalServerError

	// Attempt to unwrap our way through Temporal's WorkflowExecutionError
	// and ActivityError layers to reach the underlying cause. These types
	// contain Temporal-internal details (workflow IDs, run IDs, scheduled
	// event IDs, etc) that generally shouldn't be getting exposed to users.
	innerErr := err

	var wfErr *tp.WorkflowExecutionError
	if errors.As(innerErr, &wfErr) {
		if cause := errors.Unwrap(wfErr); cause != nil {
			innerErr = cause
		}
	}

	var actErr *tp.ActivityError
	if errors.As(innerErr, &actErr) {
		if cause := errors.Unwrap(actErr); cause != nil {
			innerErr = cause
		}
	}

	unwrappedError = innerErr

	// if the error chain contains a gRPC error code use it to tune our HTTP response code
	// NOTE: this is duplicating some feature of grpc-gateway and not exhaustive
	s, ok := status.FromError(innerErr)
	if ok {
		// NOTE: this matches WrapErr in site-workflow/pkg/error/error.go
		switch s.Code() {
		case codes.NotFound:
			code = http.StatusNotFound
		case codes.Unimplemented:
			code = http.StatusNotImplemented
		case codes.Unavailable:
			code = http.StatusServiceUnavailable
		case codes.PermissionDenied:
			code = http.StatusForbidden
		case codes.AlreadyExists:
			code = http.StatusConflict
		case codes.FailedPrecondition:
			code = http.StatusPreconditionFailed
		case codes.InvalidArgument:
			code = http.StatusBadRequest
		case codes.ResourceExhausted:
			code = http.StatusTooManyRequests
		}
	}

	// if the error is NOT a Temporal ApplicationError return what we have
	tpError := &tp.ApplicationError{}
	if !errors.As(innerErr, &tpError) {
		return
	}

	// Tune HTTP status code using the application error type coming back from Site.
	switch tpError.Type() {
	case swe.ErrTypeInvalidRequest:
		code = http.StatusBadRequest
	case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound:
		code = http.StatusNotFound
	case swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented:
		code = http.StatusNotImplemented
	case swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied:
		code = http.StatusForbidden
	case swe.ErrTypeNICoUnavailable, swe.ErrTypeCarbideUnavailable:
		code = http.StatusServiceUnavailable
	case swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists:
		code = http.StatusConflict
	case swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition:
		code = http.StatusPreconditionFailed
	case swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
		code = http.StatusBadRequest
	case swe.ErrTypeNICoResourceExhausted:
		code = http.StatusTooManyRequests
	}

	// if the error is an internal Temporal error it is mostly useless so we unwrap it but we keep
	// the current error if there is no unwrapped error
	if potentialError := errors.Unwrap(tpError); potentialError != nil {
		unwrappedError = potentialError
	}

	return
}
