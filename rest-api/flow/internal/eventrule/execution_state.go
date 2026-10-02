// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"fmt"
	"slices"
	"time"
)

// ExecutionStatus identifies one execution state.
type ExecutionStatus string

const (
	ExecutionStatusPending   ExecutionStatus = "pending"
	ExecutionStatusRunning   ExecutionStatus = "running"
	ExecutionStatusSkipped   ExecutionStatus = "skipped"
	ExecutionStatusDeferred  ExecutionStatus = "deferred"
	ExecutionStatusCompleted ExecutionStatus = "completed"
	ExecutionStatusFailed    ExecutionStatus = "failed"
)

// CanBeClaimed reports whether the scheduler may allocate an attempt for the
// execution after applying lane-specific eligibility checks.
func (s ExecutionStatus) CanBeClaimed() bool {
	return s == ExecutionStatusPending ||
		s == ExecutionStatusRunning ||
		s == ExecutionStatusDeferred
}

// RequiresRetryScheduling reports whether the status requires the store to
// calculate a next-attempt time.
func (s ExecutionStatus) RequiresRetryScheduling() bool {
	return s == ExecutionStatusDeferred
}

// ExecutionReason identifies the stable reason for an informational execution
// result without expanding the status state machine.
type ExecutionReason string

const (
	ExecutionReasonNone               ExecutionReason = ""
	ExecutionReasonNoTargets          ExecutionReason = "no_targets"
	ExecutionReasonAttemptFailed      ExecutionReason = "attempt_failed"
	ExecutionReasonAttemptInterrupted ExecutionReason = "attempt_interrupted"
)

// ExecutionStatusDetails contains the status fields shared by durable state
// and attempt results.
type ExecutionStatusDetails struct {
	Status        ExecutionStatus
	Reason        ExecutionReason
	StatusMessage string
}

// Validate checks that the status, reason, and message are internally
// consistent.
func (d ExecutionStatusDetails) Validate() error {
	reasons, ok := executionStatusReasons[d.Status]
	if !ok {
		return fmt.Errorf("unknown execution status %q", d.Status)
	}

	if len(reasons) == 0 {
		if d.Reason != ExecutionReasonNone {
			return fmt.Errorf(
				"%s execution cannot have reason %q",
				d.Status,
				d.Reason,
			)
		}
	} else if !slices.Contains(reasons, d.Reason) {
		return fmt.Errorf(
			"%s execution requires one of reasons %q",
			d.Status,
			reasons,
		)
	}

	return validateOptionalString("execution status message", d.StatusMessage)
}

// ExecutionState contains the status-dependent state of an execution.
type ExecutionState struct {
	ExecutionStatusDetails
	NextAttemptAt time.Time
}

// Validate checks that the execution state is internally consistent.
func (s ExecutionState) Validate() error {
	if err := s.ExecutionStatusDetails.Validate(); err != nil {
		return err
	}

	if s.Status.RequiresRetryScheduling() {
		if s.NextAttemptAt.IsZero() {
			return fmt.Errorf("%s execution requires next attempt time", s.Status)
		}
	} else if !s.NextAttemptAt.IsZero() {
		return fmt.Errorf("%s execution cannot have next attempt time", s.Status)
	}

	return nil
}

// RetryDue reports whether a deferred execution is eligible for the scheduler
// to dispatch at the given time.
func (s ExecutionState) RetryDue(now time.Time) bool {
	return s.Status.RequiresRetryScheduling() &&
		!s.NextAttemptAt.IsZero() &&
		!now.Before(s.NextAttemptAt)
}

var executionStatusReasons = map[ExecutionStatus][]ExecutionReason{
	ExecutionStatusPending: nil,
	ExecutionStatusRunning: nil,
	ExecutionStatusSkipped: {
		ExecutionReasonNoTargets,
	},
	ExecutionStatusDeferred: {
		ExecutionReasonAttemptFailed,
		ExecutionReasonAttemptInterrupted,
	},
	ExecutionStatusCompleted: nil,
	ExecutionStatusFailed:    nil,
}

// ExecutionResult describes the result of one dispatch attempt. Deferred
// results carry a relative delay so the store can derive NextAttemptAt from
// its authoritative clock. A zero delay makes the retry immediately eligible.
type ExecutionResult struct {
	ExecutionStatusDetails
	RetryAfter time.Duration
}

// CompletedExecutionResult creates a completed dispatch result.
func CompletedExecutionResult() ExecutionResult {
	return ExecutionResult{
		ExecutionStatusDetails: ExecutionStatusDetails{
			Status: ExecutionStatusCompleted,
		},
	}
}

// DeferredExecutionResult creates a deferred dispatch result.
func DeferredExecutionResult(
	reason ExecutionReason,
	statusMessage string,
	retryAfter time.Duration,
) ExecutionResult {
	return ExecutionResult{
		ExecutionStatusDetails: ExecutionStatusDetails{
			Status:        ExecutionStatusDeferred,
			Reason:        reason,
			StatusMessage: statusMessage,
		},
		RetryAfter: retryAfter,
	}
}

// FailedExecutionResult creates a failed dispatch result.
func FailedExecutionResult(statusMessage string) ExecutionResult {
	return ExecutionResult{
		ExecutionStatusDetails: ExecutionStatusDetails{
			Status:        ExecutionStatusFailed,
			StatusMessage: statusMessage,
		},
	}
}

// Validate checks that the dispatch result is internally consistent.
func (r ExecutionResult) Validate() error {
	if r.Status == ExecutionStatusPending ||
		r.Status == ExecutionStatusRunning ||
		r.Status == ExecutionStatusSkipped {
		return fmt.Errorf("%s is not an execution result", r.Status)
	}

	if err := r.ExecutionStatusDetails.Validate(); err != nil {
		return err
	}

	if r.Status.RequiresRetryScheduling() {
		if r.RetryAfter < 0 {
			return fmt.Errorf("deferred execution retry delay cannot be negative")
		}
	} else if r.RetryAfter != 0 {
		return fmt.Errorf("%s execution cannot have retry delay", r.Status)
	}

	return nil
}

func (r ExecutionResult) stateAt(now time.Time) ExecutionState {
	state := ExecutionState{
		ExecutionStatusDetails: r.ExecutionStatusDetails,
	}
	if r.Status.RequiresRetryScheduling() {
		state.NextAttemptAt = now.Add(r.RetryAfter)
	}

	return state
}
