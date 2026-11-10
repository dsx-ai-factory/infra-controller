// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

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

type proxyInvocation struct {
	stc        tclient.Client
	backend    grpcproxy.Backend
	fullMethod string
	request    grpcproxy.Request
}

func newProxyInvocation(stc tclient.Client, backend grpcproxy.Backend, fullMethod string, req proto.Message, secretKey string, secretFields ...string) (*proxyInvocation, *cutil.APIError) {
	// Named secrets must never reach Temporal history without encryption.
	if len(secretFields) > 0 && secretKey == "" {
		return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to encode %s proxy request", backend.Label), fmt.Errorf("secret fields named for %s without a secret key", fullMethod))
	}
	reqJSON, err := protojson.Marshal(req)
	if err != nil {
		return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to encode %s proxy request", backend.Label), fmt.Errorf("encode request for %s: %w", fullMethod, err))
	}

	var encryptedSecrets []byte
	if len(secretFields) > 0 {
		redacted, secretsJSON, err := grpcproxy.RedactSecrets(reqJSON, secretFields)
		if err != nil {
			return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to redact %s proxy request", backend.Label), err)
		}
		reqJSON = redacted
		if len(secretsJSON) > 0 {
			encryptedSecrets = cutil.EncryptData(secretsJSON, secretKey)
		}
	}
	return &proxyInvocation{
		stc:        stc,
		backend:    backend,
		fullMethod: fullMethod,
		request: grpcproxy.Request{
			FullMethod:       fullMethod,
			RequestJSON:      reqJSON,
			EncryptedSecrets: encryptedSecrets,
		},
	}, nil
}

// execute never terminates an abandoned workflow: its backend RPC may still be
// running, and deterministic Flow IDs must remain available for retries to join.
// WorkflowExecutionTimeout and ActivityStartToCloseTimeout bound that execution.
func (p *proxyInvocation) execute(ctx context.Context, resp proto.Message, workflowID string, conflictPolicy temporalEnums.WorkflowIdConflictPolicy) *cutil.APIError {
	wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	we, err := p.stc.ExecuteWorkflow(wfCtx, p.startOptions(workflowID, conflictPolicy), p.backend.WorkflowName, p.request)
	if err != nil {
		return p.startError(wfCtx, err)
	}
	var out grpcproxy.Response
	resultErr := we.Get(wfCtx, &out)
	if resultErr != nil {
		timeoutErr := p.resultTimeout(wfCtx, resultErr)
		if timeoutErr != nil {
			return timeoutErr
		}
		code, werr := UnwrapWorkflowError(resultErr)
		return cutil.NewAPIError(code, werr.Error(), nil).WithCause(resultErr)
	}
	return p.decode(out, resp)
}

func (p *proxyInvocation) executeAttempt(ctx context.Context, resp proto.Message, workflowID string, startNewAttempt bool) (*cutil.APIError, bool) {
	wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	we, err := p.startAttempt(wfCtx, workflowID, startNewAttempt)
	if err != nil {
		return p.startError(wfCtx, err), false
	}
	var out grpcproxy.Response
	resultErr := we.Get(wfCtx, &out)
	if resultErr != nil {
		timeoutErr := p.resultTimeout(wfCtx, resultErr)
		if timeoutErr != nil {
			return timeoutErr, false
		}
		return attemptFailure(resultErr)
	}
	if !json.Valid(out.ResponseJSON) {
		return cutil.NewAPIError(http.StatusInternalServerError, "Core proxy result is missing or malformed", nil), false
	}
	apiErr := p.decode(out, resp)
	return apiErr, apiErr == nil
}

func (p *proxyInvocation) startAttempt(ctx context.Context, workflowID string, startNewAttempt bool) (tclient.WorkflowRun, error) {
	if !startNewAttempt {
		return p.stc.GetWorkflow(ctx, workflowID, ""), nil
	}
	options := p.startOptions(workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_FAIL)
	options.WorkflowIDReusePolicy = temporalEnums.WORKFLOW_ID_REUSE_POLICY_REJECT_DUPLICATE
	options.WorkflowExecutionErrorWhenAlreadyStarted = true
	we, err := p.stc.ExecuteWorkflow(ctx, options, p.backend.WorkflowName, p.request)
	var alreadyStarted *serviceerror.WorkflowExecutionAlreadyStarted
	if errors.As(err, &alreadyStarted) {
		return p.stc.GetWorkflow(ctx, workflowID, alreadyStarted.RunId), nil
	}
	return we, err
}

func (p *proxyInvocation) startOptions(workflowID string, conflictPolicy temporalEnums.WorkflowIdConflictPolicy) tclient.StartWorkflowOptions {
	return tclient.StartWorkflowOptions{
		ID:                       workflowID,
		WorkflowExecutionTimeout: grpcproxy.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
		WorkflowIDReusePolicy:    temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
		WorkflowIDConflictPolicy: conflictPolicy,
	}
}

func (p *proxyInvocation) startError(ctx context.Context, err error) *cutil.APIError {
	// Either deadline can occur after the server accepted the start. The SDK's
	// shorter start deadline can expire while the caller is still waiting.
	if ctx.Err() != nil {
		return p.timedOut("caller stopped waiting", ctx.Err())
	}
	var startDeadlineErr *serviceerror.DeadlineExceeded
	if errors.As(err, &startDeadlineErr) {
		return p.timedOut("start deadline exceeded", err)
	}
	return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute %s proxy workflow", p.backend.Label), fmt.Errorf("execute %s workflow: %w", p.backend.WorkflowName, err))
}

func (p *proxyInvocation) resultTimeout(ctx context.Context, resultErr error) *cutil.APIError {
	// A Temporal timeout proves execution closed, even if the caller also left.
	var timeoutErr *tp.TimeoutError
	if errors.As(resultErr, &timeoutErr) {
		return p.timedOut("execution timed out", resultErr)
	}
	// A result's DeadlineExceeded can come from inside the execution. Only this
	// context proves the caller stopped waiting for the result.
	if ctx.Err() != nil {
		return p.timedOut("caller stopped waiting", ctx.Err())
	}
	return nil
}

func (p *proxyInvocation) timedOut(detail string, cause error) *cutil.APIError {
	return cutil.NewAPIError(http.StatusGatewayTimeout, fmt.Sprintf("%s proxy request timed out", p.backend.Label), fmt.Errorf("%s proxy %s %s: %w", strings.ToLower(p.backend.Label), p.fullMethod, detail, cause))
}

func (p *proxyInvocation) decode(out grpcproxy.Response, resp proto.Message) *cutil.APIError {
	if resp != nil && len(out.ResponseJSON) > 0 {
		err := (protojson.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(out.ResponseJSON, resp)
		if err != nil {
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to decode %s proxy response", p.backend.Label), fmt.Errorf("decode response for %s: %w", p.fullMethod, err))
		}
	}
	return nil
}
