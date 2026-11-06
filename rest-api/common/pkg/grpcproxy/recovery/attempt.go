// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package recovery verifies original Core proxy executions for REST callers.
// It is separate from the proxy transport contract so on-site services can use
// that contract without importing REST recovery and Site workflow error types.
package recovery

import (
	"bytes"
	"context"
	"errors"
	"fmt"

	enumspb "go.temporal.io/api/enums/v1"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// CoreAttemptResult distinguishes proven Core replies from an unresolved attempt.
type CoreAttemptResult uint8

const (
	// CoreAttemptUnknown retains the fence until the original result can be proved.
	CoreAttemptUnknown CoreAttemptResult = iota
	// CoreAttemptSucceeded is a decoded success from the exact original run.
	CoreAttemptSucceeded
	// CoreAttemptNotFound is a verified first-attempt Core absence reply.
	CoreAttemptNotFound
	// CoreAttemptRejected proves the original first activity rejected the request.
	CoreAttemptRejected
)

// ResolveCoreAttemptRunID returns the recorded run, or discovers an unbound
// original without starting anything. Discovery requires complete authoritative
// history identifying this workflow's first run and excluding reset/continuation
// or an existing successor. bindRun must conditionally persist that identity
// before this function returns it. Missing proof leaves the attempt unresolved.
func ResolveCoreAttemptRunID(ctx context.Context, stc client.Client, workflowID, runID string, bindRun func(string) error) (string, error) {
	if workflowID == "" {
		return "", errors.New("core attempt requires a workflow ID")
	}
	if runID != "" {
		return runID, nil
	}
	if bindRun == nil {
		return "", errors.New("core attempt has no durable original run binding")
	}
	execution, err := stc.DescribeWorkflowExecution(ctx, workflowID, "")
	if err != nil {
		return "", err
	}
	info := execution.GetWorkflowExecutionInfo()
	candidate := info.GetExecution().GetRunId()
	if candidate == "" || info.GetType().GetName() != grpcproxy.Core.WorkflowName {
		return "", errors.New("core attempt original execution is unverified")
	}
	history := stc.GetWorkflowHistory(ctx, workflowID, candidate, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT)
	if history == nil {
		return "", errors.New("core attempt original history is missing")
	}
	lastID := int64(0)
	closed := false
	for history.HasNext() {
		if ctx.Err() != nil {
			return "", ctx.Err()
		}
		event, err := history.Next()
		if err != nil {
			return "", err
		}
		if event == nil || closed || event.GetEventId() != lastID+1 {
			return "", errors.New("core attempt original history is incomplete")
		}
		lastID = event.GetEventId()
		if lastID == 1 {
			start := event.GetWorkflowExecutionStartedEventAttributes()
			if event.GetEventType() != enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED || start == nil || start.GetWorkflowType().GetName() != grpcproxy.Core.WorkflowName ||
				start.GetOriginalExecutionRunId() != candidate || start.GetFirstExecutionRunId() != candidate || start.GetContinuedExecutionRunId() != "" || start.GetAttempt() != 1 || start.GetCronSchedule() != "" ||
				(start.GetWorkflowId() != "" && start.GetWorkflowId() != workflowID) {
				return "", errors.New("core attempt original run is unverified")
			}
		}
		successor := false
		switch event.GetEventType() {
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_CONTINUED_AS_NEW:
			successor = true
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_COMPLETED:
			a := event.GetWorkflowExecutionCompletedEventAttributes()
			closed = a != nil
			successor = a == nil || a.GetNewExecutionRunId() != ""
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED:
			a := event.GetWorkflowExecutionFailedEventAttributes()
			closed = a != nil
			successor = a == nil || a.GetNewExecutionRunId() != "" || a.GetRetryState() == enumspb.RETRY_STATE_IN_PROGRESS
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT:
			a := event.GetWorkflowExecutionTimedOutEventAttributes()
			closed = a != nil
			successor = a == nil || a.GetNewExecutionRunId() != "" || a.GetRetryState() == enumspb.RETRY_STATE_IN_PROGRESS
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_CANCELED:
			closed = event.GetWorkflowExecutionCanceledEventAttributes() != nil
			successor = !closed
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TERMINATED:
			closed = event.GetWorkflowExecutionTerminatedEventAttributes() != nil
			successor = !closed
		case enumspb.EVENT_TYPE_WORKFLOW_TASK_FAILED:
			a := event.GetWorkflowTaskFailedEventAttributes()
			successor = a == nil || a.GetCause() == enumspb.WORKFLOW_TASK_FAILED_CAUSE_RESET_WORKFLOW || a.GetBaseRunId() != "" || a.GetNewRunId() != "" || a.GetForkEventVersion() != 0
		}
		if successor {
			return "", errors.New("core attempt has a reset or successor execution")
		}
	}
	if ctx.Err() != nil {
		return "", ctx.Err()
	}
	if lastID == 0 || (!closed && info.GetStatus() != enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING) {
		return "", errors.New("core attempt original history is incomplete")
	}
	latest, err := stc.DescribeWorkflowExecution(ctx, workflowID, "")
	if err != nil {
		return "", err
	}
	if latest.GetWorkflowExecutionInfo().GetExecution().GetRunId() != candidate || latest.GetWorkflowExecutionInfo().GetStatus() != info.GetStatus() {
		return "", errors.New("core attempt execution changed during original run verification")
	}
	if ctx.Err() != nil {
		return "", ctx.Err()
	}
	err = bindRun(candidate)
	if err != nil {
		return "", err
	}
	return candidate, nil
}

// ObserveCoreAttempt reads only the durably bound original run. When unbound,
// it verifies and persists an original through bindRun first. It never starts
// an execution or follows another run. resp must be nonnil and typed, including
// &emptypb.Empty{} for an empty reply. Inspect the classification before err:
// verified NotFound and Rejected replies return their nonnil original error.
func ObserveCoreAttempt(ctx context.Context, stc client.Client, workflowID, runID string, resp proto.Message, bindRun func(string) error) (CoreAttemptResult, error) {
	runID, err := ResolveCoreAttemptRunID(ctx, stc, workflowID, runID, bindRun)
	if err != nil {
		return CoreAttemptUnknown, err
	}
	var response grpcproxy.Response
	err = stc.GetWorkflow(ctx, workflowID, runID).GetWithOptions(ctx, &response, client.WorkflowRunGetOptions{DisableFollowingRuns: true})
	return CheckCoreAttemptResult(ctx, stc, workflowID, runID, response, resp, err)
}

// CheckCoreAttemptResult validates an exact original run's result. Typed Core
// failures also require its first activity attempt and no existing successor;
// an HTTP code or a Temporal NotFound is never proof of Core completion.
// resp must be nonnil and typed (&emptypb.Empty{} for an empty reply).
// NotFound and Rejected return a conclusive classification with a nonnil error;
// only Succeeded returns nil. Unknown always retains the attempt.
func CheckCoreAttemptResult(ctx context.Context, stc client.Client, workflowID, runID string, response grpcproxy.Response, resp proto.Message, resultErr error) (CoreAttemptResult, error) {
	if resultErr == nil {
		data := bytes.TrimSpace(response.ResponseJSON)
		if len(data) == 0 || data[0] != '{' || resp == nil {
			return CoreAttemptUnknown, errors.New("core proxy result is missing or malformed")
		}
		err := protojson.Unmarshal(data, resp)
		if err != nil {
			return CoreAttemptUnknown, fmt.Errorf("decode original Core proxy response: %w", err)
		}
		return CoreAttemptSucceeded, nil
	}
	var timeout *temporal.TimeoutError
	var canceled *temporal.CanceledError
	var activity *temporal.ActivityError
	var application *temporal.ApplicationError
	if errors.As(resultErr, &timeout) || errors.As(resultErr, &canceled) ||
		!errors.As(resultErr, &activity) || activity.ActivityType().GetName() != "InvokeCoreGRPCOnSite" || !errors.As(resultErr, &application) {
		return CoreAttemptUnknown, resultErr
	}
	result := CoreAttemptUnknown
	switch application.Type() {
	case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound:
		result = CoreAttemptNotFound
	case swe.ErrTypeInvalidRequest,
		swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented,
		swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied,
		swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists,
		swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition,
		swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
		result = CoreAttemptRejected
	}
	if result == CoreAttemptUnknown {
		return result, resultErr
	}
	verified, err := verifyCoreAttemptFailure(ctx, stc, workflowID, runID, activity)
	if err != nil {
		return CoreAttemptUnknown, err
	}
	if !verified {
		return CoreAttemptUnknown, resultErr
	}
	return result, resultErr
}

func verifyCoreAttemptFailure(ctx context.Context, stc client.Client, workflowID, runID string, activity *temporal.ActivityError) (bool, error) {
	if workflowID == "" || runID == "" || activity.ScheduledEventID() <= 0 || activity.StartedEventID() <= activity.ScheduledEventID() || activity.ActivityID() == "" {
		return false, nil
	}
	execution, err := stc.DescribeWorkflowExecution(ctx, workflowID, "")
	if err != nil {
		return false, err
	}
	info := execution.GetWorkflowExecutionInfo()
	if info.GetExecution().GetRunId() != runID || info.GetStatus() != enumspb.WORKFLOW_EXECUTION_STATUS_FAILED {
		return false, nil
	}
	history := stc.GetWorkflowHistory(ctx, workflowID, runID, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT)
	if history == nil {
		return false, nil
	}
	original, scheduled, started := false, false, false
	for history.HasNext() {
		if ctx.Err() != nil {
			return false, ctx.Err()
		}
		event, err := history.Next()
		if err != nil {
			return false, err
		}
		if event == nil {
			return false, nil
		}
		if event.GetEventId() == 1 {
			start := event.GetWorkflowExecutionStartedEventAttributes()
			original = event.GetEventType() == enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED && start != nil && start.GetWorkflowType().GetName() == grpcproxy.Core.WorkflowName && start.GetOriginalExecutionRunId() == runID && start.GetFirstExecutionRunId() == runID && start.GetContinuedExecutionRunId() == "" && start.GetAttempt() == 1 && start.GetCronSchedule() == ""
			if !original {
				return false, nil
			}
		}
		switch event.GetEventId() {
		case activity.ScheduledEventID():
			a := event.GetActivityTaskScheduledEventAttributes()
			scheduled = event.GetEventType() == enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED && a != nil && a.GetActivityType().GetName() == activity.ActivityType().GetName() && a.GetActivityId() == activity.ActivityID()
			if !scheduled {
				return false, nil
			}
		case activity.StartedEventID():
			a := event.GetActivityTaskStartedEventAttributes()
			started = event.GetEventType() == enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED && a != nil && a.GetScheduledEventId() == activity.ScheduledEventID() && a.GetAttempt() == 1
			if !started {
				return false, nil
			}
		}
		if event.GetEventType() == enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED {
			closed := event.GetWorkflowExecutionFailedEventAttributes()
			return original && scheduled && started && closed != nil && closed.GetNewExecutionRunId() == "" && closed.GetRetryState() != enumspb.RETRY_STATE_IN_PROGRESS, ctx.Err()
		}
	}
	return false, ctx.Err()
}
