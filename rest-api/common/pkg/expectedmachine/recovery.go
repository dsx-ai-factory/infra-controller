// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package expectedmachine verifies batch creation results for the REST API and
// inventory recovery.
package expectedmachine

import (
	"context"
	"errors"

	enumspb "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// IsExpectedMachineBatchRejectionCandidate identifies a typed create rejection.
// It does not establish which activity attempt failed; clearing a reservation
// also requires VerifyExpectedMachineBatchRejection.
func IsExpectedMachineBatchRejectionCandidate(err error) bool {
	var timeout *temporal.TimeoutError
	var canceled *temporal.CanceledError
	if errors.As(err, &timeout) || errors.As(err, &canceled) {
		return false
	}
	var activity *temporal.ActivityError
	var application *temporal.ApplicationError
	if !errors.As(err, &activity) || activity.ActivityType().GetName() != "CreateExpectedMachinesOnSite" || !errors.As(err, &application) {
		return false
	}
	switch application.Type() {
	case swe.ErrTypeInvalidRequest,
		swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound,
		swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented,
		swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied,
		swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists,
		swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition,
		swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
		return true
	}
	return false
}

// VerifyExpectedMachineBatchRejection requires positive history evidence that
// the exact failed activity was its first attempt in the specified run. An older
// Site worker may have retried after a Core commit, so a later rejection alone
// cannot release the reservation. The original must still be the latest failed
// run; an existing replacement can still create. Missing history never proves non-dispatch.
// The caller supplies the existing observation deadline through ctx.
func VerifyExpectedMachineBatchRejection(ctx context.Context, stc client.Client, workflowID, runID string, workflowErr error) (bool, error) {
	if workflowID == "" || runID == "" || !IsExpectedMachineBatchRejectionCandidate(workflowErr) {
		return false, nil
	}
	var activity *temporal.ActivityError
	if !errors.As(workflowErr, &activity) || activity.ScheduledEventID() <= 0 || activity.StartedEventID() <= activity.ScheduledEventID() || activity.ActivityID() == "" {
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
	scheduled, started := false, false
	for {
		if ctx.Err() != nil {
			return false, ctx.Err()
		}
		if !history.HasNext() {
			return false, ctx.Err()
		}
		event, err := history.Next()
		if err != nil {
			return false, err
		}
		if event == nil {
			return false, nil
		}
		if ctx.Err() != nil {
			return false, ctx.Err()
		}
		switch event.GetEventId() {
		case activity.ScheduledEventID():
			attributes := event.GetActivityTaskScheduledEventAttributes()
			if event.GetEventType() != enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED || attributes == nil || attributes.GetActivityType().GetName() != activity.ActivityType().GetName() || attributes.GetActivityId() != activity.ActivityID() {
				return false, nil
			}
			scheduled = true
		case activity.StartedEventID():
			attributes := event.GetActivityTaskStartedEventAttributes()
			if event.GetEventType() != enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED || attributes == nil || attributes.GetScheduledEventId() != activity.ScheduledEventID() || attributes.GetAttempt() != 1 {
				return false, nil
			}
			started = true
		}
		if scheduled && started {
			return true, nil
		}
	}
}

// VerifyExpectedMachineBatchNotDispatched requires complete history from the
// acknowledged original run. A closed run with no scheduled remote activity
// cannot have invoked Core. Unexpected commands, lineage, or missing history
// retain the reservation; absence of an activity start alone proves nothing.
func VerifyExpectedMachineBatchNotDispatched(ctx context.Context, stc client.Client, workflowID, runID string) (bool, error) {
	if workflowID == "" || runID == "" {
		return false, nil
	}
	if ctx.Err() != nil {
		return false, ctx.Err()
	}
	execution, err := stc.DescribeWorkflowExecution(ctx, workflowID, "")
	if err != nil {
		return false, err
	}
	info := execution.GetWorkflowExecutionInfo()
	if info.GetExecution().GetRunId() != runID {
		return false, nil
	}
	var terminal enumspb.EventType
	switch info.GetStatus() {
	case enumspb.WORKFLOW_EXECUTION_STATUS_FAILED:
		terminal = enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED
	case enumspb.WORKFLOW_EXECUTION_STATUS_TIMED_OUT:
		terminal = enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT
	case enumspb.WORKFLOW_EXECUTION_STATUS_CANCELED:
		terminal = enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_CANCELED
	case enumspb.WORKFLOW_EXECUTION_STATUS_TERMINATED:
		terminal = enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TERMINATED
	default:
		return false, nil
	}
	history := stc.GetWorkflowHistory(ctx, workflowID, runID, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT)
	if history == nil {
		return false, nil
	}
	lastID := int64(0)
	closed := false
	for {
		if ctx.Err() != nil {
			return false, ctx.Err()
		}
		if !history.HasNext() {
			if ctx.Err() != nil {
				return false, ctx.Err()
			}
			return closed && lastID > 1, nil
		}
		event, err := history.Next()
		if err != nil {
			return false, err
		}
		if event == nil || closed || event.GetEventId() != lastID+1 {
			return false, nil
		}
		lastID = event.GetEventId()
		if lastID == 1 {
			start := event.GetWorkflowExecutionStartedEventAttributes()
			if event.GetEventType() != enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED || start == nil || start.GetWorkflowType().GetName() != "CreateExpectedMachines" ||
				start.GetOriginalExecutionRunId() != runID || start.GetFirstExecutionRunId() != runID || start.GetContinuedExecutionRunId() != "" ||
				(start.GetWorkflowId() != "" && start.GetWorkflowId() != workflowID) || start.GetAttempt() != 1 || start.GetCronSchedule() != "" {
				return false, nil
			}
			continue
		}
		switch event.GetEventType() {
		case enumspb.EVENT_TYPE_WORKFLOW_TASK_SCHEDULED:
			if event.GetWorkflowTaskScheduledEventAttributes() == nil {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_TASK_STARTED:
			if event.GetWorkflowTaskStartedEventAttributes() == nil {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_TASK_COMPLETED:
			if event.GetWorkflowTaskCompletedEventAttributes() == nil {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_TASK_FAILED:
			attributes := event.GetWorkflowTaskFailedEventAttributes()
			if attributes == nil || attributes.GetCause() == enumspb.WORKFLOW_TASK_FAILED_CAUSE_RESET_WORKFLOW || attributes.GetBaseRunId() != "" || attributes.GetNewRunId() != "" || attributes.GetForkEventVersion() != 0 {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_TASK_TIMED_OUT:
			if event.GetWorkflowTaskTimedOutEventAttributes() == nil {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED:
			attributes := event.GetWorkflowExecutionFailedEventAttributes()
			if attributes == nil || attributes.GetNewExecutionRunId() != "" || attributes.GetRetryState() == enumspb.RETRY_STATE_IN_PROGRESS {
				return false, nil
			}
			closed = event.GetEventType() == terminal
			if !closed {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT:
			attributes := event.GetWorkflowExecutionTimedOutEventAttributes()
			if attributes == nil || attributes.GetNewExecutionRunId() != "" || attributes.GetRetryState() == enumspb.RETRY_STATE_IN_PROGRESS {
				return false, nil
			}
			closed = event.GetEventType() == terminal
			if !closed {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_CANCELED:
			closed = event.GetEventType() == terminal && event.GetWorkflowExecutionCanceledEventAttributes() != nil
			if !closed {
				return false, nil
			}
		case enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TERMINATED:
			closed = event.GetEventType() == terminal && event.GetWorkflowExecutionTerminatedEventAttributes() != nil
			if !closed {
				return false, nil
			}
		default:
			// This rejects activities of every type, local-activity markers,
			// children, signals, timers, and successors without interpreting them.
			return false, nil
		}
	}
}

// ExpectedMachineBatchFailureCategory returns bounded diagnostic text without
// exposing Temporal error messages, payloads, or caller-supplied error type names.
func ExpectedMachineBatchFailureCategory(err error) string {
	if err == nil {
		return "malformed_result"
	}
	if errors.Is(err, context.Canceled) {
		return "caller_canceled"
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return "deadline_exceeded"
	}
	var timeout *temporal.TimeoutError
	var canceled *temporal.CanceledError
	if errors.As(err, &timeout) {
		return "workflow_timeout"
	}
	if errors.As(err, &canceled) {
		return "workflow_canceled"
	}
	var missing *serviceerror.NotFound
	var namespace *serviceerror.NamespaceNotFound
	var unavailable *serviceerror.Unavailable
	var deadline *serviceerror.DeadlineExceeded
	if errors.As(err, &missing) {
		return "history_missing"
	}
	if errors.As(err, &namespace) {
		return "namespace_missing"
	}
	if errors.As(err, &unavailable) {
		return "service_unavailable"
	}
	if errors.As(err, &deadline) {
		return "deadline_exceeded"
	}
	var application *temporal.ApplicationError
	if IsExpectedMachineBatchRejectionCandidate(err) && errors.As(err, &application) {
		switch application.Type() {
		case swe.ErrTypeInvalidRequest:
			return "invalid_request"
		case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound:
			return "core_not_found"
		case swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented:
			return "core_unimplemented"
		case swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied:
			return "core_permission_denied"
		case swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists:
			return "core_already_exists"
		case swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition:
			return "core_failed_precondition"
		case swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
			return "core_invalid_argument"
		}
	}
	if errors.As(err, &application) {
		return "execution_failed"
	}
	return "observation_failed"
}
