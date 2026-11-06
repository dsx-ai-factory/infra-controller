// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"github.com/google/uuid"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// HandlingStatus describes the overall progress of all applicable actions and Tasks.
type HandlingStatus string

const (
	HandlingStatusUnknown   HandlingStatus = "unknown"
	HandlingStatusNoAction  HandlingStatus = "no_action"
	HandlingStatusStarting  HandlingStatus = "starting"
	HandlingStatusRetrying  HandlingStatus = "retrying"
	HandlingStatusRunning   HandlingStatus = "running"
	HandlingStatusCompleted HandlingStatus = "completed"
	HandlingStatusSkipped   HandlingStatus = "skipped"
	HandlingStatusFailed    HandlingStatus = "failed"
)

// HandlingReason is a bounded diagnostic code. Empty means no diagnostic is needed.
type HandlingReason string

const (
	HandlingReasonNoTargets          HandlingReason = "no_targets"
	HandlingReasonAttemptFailed      HandlingReason = "attempt_failed"
	HandlingReasonAttemptInterrupted HandlingReason = "attempt_interrupted"
	HandlingReasonRetryScheduled     HandlingReason = "retry_scheduled"
	HandlingReasonActionFailed       HandlingReason = "action_failed"
	HandlingReasonTaskFailed         HandlingReason = "task_failed"
	HandlingReasonIncompleteState    HandlingReason = "incomplete_state"
)

// HandlingResult is Flow's authoritative summary, independent of transport and event type.
// Message is curated; backend error text is never included.
type HandlingResult struct {
	Status  HandlingStatus
	Reason  HandlingReason
	Message string
}

// Result aggregates the captured policy, executions, and their Tasks. Precedence is
// Failed > Retrying > Starting > Unknown > Running > Skipped > Completed > NoAction.
// A completed dispatch is insufficient without Tasks for every submit-task action.
// Missing/duplicate matching executions and unknown states remain Unknown. Unrelated
// executions cannot satisfy a captured action. No applicable actions means NoAction.
func (h Handling) Result() HandlingResult {
	result := HandlingResult{Status: HandlingStatusNoAction}
	for _, task := range h.Tasks {
		if task.Status == taskcommon.TaskStatusFailed || task.Status == taskcommon.TaskStatusTerminated {
			result = HandlingResult{Status: HandlingStatusFailed, Reason: HandlingReasonTaskFailed}
			result.Message = result.message()
			return result
		}
	}
	for _, action := range h.AppliedPolicy.Actions {
		candidate := h.actionResult(action)
		if candidate.priority() > result.priority() {
			result = candidate
		}
	}
	result.Message = result.message()
	return result
}

func (h Handling) actionResult(action Action) HandlingResult {
	unknown := HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState}
	var execution *HandlingExecution
	for i := range h.Executions {
		if h.Executions[i].ActionName == action.Name {
			if execution != nil {
				return unknown
			}
			execution = &h.Executions[i]
		}
	}
	if execution == nil {
		return unknown
	}
	switch execution.Status {
	case ExecutionStatusFailed:
		return HandlingResult{Status: HandlingStatusFailed, Reason: HandlingReasonActionFailed}
	case ExecutionStatusDeferred:
		reason := HandlingReasonRetryScheduled
		switch execution.Reason {
		case ExecutionReasonAttemptFailed:
			reason = HandlingReasonAttemptFailed
		case ExecutionReasonAttemptInterrupted:
			reason = HandlingReasonAttemptInterrupted
		}
		return HandlingResult{Status: HandlingStatusRetrying, Reason: reason}
	case ExecutionStatusPending, ExecutionStatusRunning:
		return HandlingResult{Status: HandlingStatusStarting}
	case ExecutionStatusSkipped:
		reason := HandlingReason("")
		if execution.Reason == ExecutionReasonNoTargets {
			reason = HandlingReasonNoTargets
		}
		return HandlingResult{Status: HandlingStatusSkipped, Reason: reason}
	case ExecutionStatusCompleted:
		switch action.Spec.(type) {
		case *SubmitTask:
			return h.submittedTaskResult(execution.ID)
		case *SendAlert:
			return HandlingResult{Status: HandlingStatusCompleted}
		case *Noop:
			return HandlingResult{Status: HandlingStatusNoAction}
		}
	}
	return unknown
}

func (h Handling) submittedTaskResult(executionID uuid.UUID) HandlingResult {
	result := HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState}
	found, active, unknown := false, false, false
	for _, task := range h.Tasks {
		if executionID == uuid.Nil || task.ExecutionID != executionID {
			continue
		}
		found = true
		switch task.Status {
		case taskcommon.TaskStatusWaiting, taskcommon.TaskStatusPending, taskcommon.TaskStatusRunning:
			active = true
		case taskcommon.TaskStatusCompleted:
		default:
			unknown = true
		}
	}
	if !found || unknown {
		return result
	}
	if active {
		return HandlingResult{Status: HandlingStatusRunning}
	}
	return HandlingResult{Status: HandlingStatusCompleted}
}

func (r HandlingResult) priority() int {
	switch r.Status {
	case HandlingStatusFailed:
		return 100
	case HandlingStatusRetrying:
		// Choose the same diagnostic regardless of action/execution ordering.
		switch r.Reason {
		case HandlingReasonAttemptFailed:
			return 92
		case HandlingReasonAttemptInterrupted:
			return 91
		default:
			return 90
		}
	case HandlingStatusStarting:
		return 80
	case HandlingStatusUnknown:
		return 70
	case HandlingStatusRunning:
		return 60
	case HandlingStatusSkipped:
		if r.Reason == HandlingReasonNoTargets {
			return 51
		}
		return 50
	case HandlingStatusCompleted:
		return 40
	default:
		return 0
	}
}

func (r HandlingResult) message() string {
	switch r.Status {
	case HandlingStatusFailed:
		if r.Reason == HandlingReasonTaskFailed {
			return "One or more handling tasks failed or were terminated."
		}
		return "One or more handling actions could not be completed."
	case HandlingStatusRetrying:
		switch r.Reason {
		case HandlingReasonAttemptFailed:
			return "A handling action failed; another attempt is scheduled."
		case HandlingReasonAttemptInterrupted:
			return "A handling attempt was interrupted; another attempt is scheduled."
		default:
			return "Another handling attempt is scheduled."
		}
	case HandlingStatusStarting:
		return "Handling actions are being started."
	case HandlingStatusUnknown:
		return "Handling progress is unavailable because execution or task information is incomplete."
	case HandlingStatusRunning:
		return "Handling tasks are in progress."
	case HandlingStatusSkipped:
		if r.Reason == HandlingReasonNoTargets {
			return "One or more handling actions had no eligible targets."
		}
		return "One or more handling actions were skipped."
	case HandlingStatusCompleted:
		return "Handling actions and their tasks completed successfully."
	default:
		return "No action was required by the applied policy."
	}
}
