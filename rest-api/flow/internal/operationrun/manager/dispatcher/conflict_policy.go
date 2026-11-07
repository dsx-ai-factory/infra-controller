// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dispatcher

import (
	"encoding/json"
	"errors"
	"fmt"
	"time"

	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
)

type conflictPolicyRuntime interface {
	evaluate(
		targets []*operationrun.OperationRunTarget,
		now time.Time,
	) pauseDecision
	handleSubmissionError(
		target *operationrun.OperationRunTarget,
		err error,
		now time.Time,
	) bool
	waitingStatusMessage() string
}

func newConflictPolicy(options *operationrun.Options) (conflictPolicyRuntime, error) {
	payload := options.ConflictPolicy.Payload
	if payload == nil {
		return nil, fmt.Errorf("conflict policy is required")
	}

	switch policy := payload.(type) {
	case *operationrun.ConflictRetryPolicy:
		return retryConflictPolicy{policy: policy}, nil
	default:
		return nil, fmt.Errorf("unsupported conflict policy kind %q", payload.ConflictPolicyKind())
	}
}

type retryConflictPolicy struct {
	policy *operationrun.ConflictRetryPolicy
}

func (p retryConflictPolicy) evaluate(
	targets []*operationrun.OperationRunTarget,
	now time.Time,
) pauseDecision {
	timedOut, msg := conflictRetryTimedOut(targets, p.policy, now)
	if !timedOut {
		return pauseDecision{
			pause: false,
		}
	}

	return pauseDecision{
		pause:   true,
		reason:  operationrun.OperationRunStatusReasonConflictRetryTimeout,
		message: msg,
	}
}

func (p retryConflictPolicy) handleSubmissionError(
	target *operationrun.OperationRunTarget,
	err error,
	now time.Time,
) bool {
	if !errors.Is(err, taskmanager.ErrRackConflict) {
		return false
	}

	blockTarget(target, p.policy, now, err.Error())
	return true
}

func (retryConflictPolicy) waitingStatusMessage() string {
	return "waiting on rack conflicts"
}

// blockTarget records a retryable rack conflict and calculates the next
// exponential-backoff retry time.
func blockTarget(
	target *operationrun.OperationRunTarget,
	policy *operationrun.ConflictRetryPolicy,
	now time.Time,
	message string,
) {
	state := decodeConflictRetryState(target.RetryState)
	if state.BlockedSince.IsZero() {
		state.BlockedSince = now
	}
	if state.LastRetryDelaySeconds <= 0 {
		state.LastRetryDelaySeconds = int64(policy.InitialRetryDelay.Seconds())
	} else {
		state.LastRetryDelaySeconds *= 2
		maxSeconds := int64(policy.MaxRetryDelay.Seconds())
		if state.LastRetryDelaySeconds > maxSeconds {
			state.LastRetryDelaySeconds = maxSeconds
		}
	}
	state.Attempts++

	delay := time.Duration(state.LastRetryDelaySeconds) * time.Second
	retryAfter := now.Add(delay)
	raw, _ := json.Marshal(state)

	target.Block(message, retryAfter, raw)
}

// conflictRetryState is persisted on blocked targets so retry backoff survives
// dispatcher restarts.
type conflictRetryState struct {
	BlockedSince          time.Time `json:"blocked_since"`
	Attempts              int       `json:"attempts"`
	LastRetryDelaySeconds int64     `json:"last_retry_delay_seconds"`
}

// decodeConflictRetryState tolerates missing or malformed retry state by
// treating the next block as the first retry attempt.
func decodeConflictRetryState(raw json.RawMessage) conflictRetryState {
	if len(raw) == 0 {
		return conflictRetryState{}
	}

	var state conflictRetryState
	if err := json.Unmarshal(raw, &state); err != nil {
		return conflictRetryState{}
	}

	return state
}

// conflictRetryTimedOut pauses the run when any blocked target has
// exceeded the configured rack-conflict retry window.
func conflictRetryTimedOut(
	targets []*operationrun.OperationRunTarget,
	policy *operationrun.ConflictRetryPolicy,
	now time.Time,
) (bool, string) {
	for _, target := range targets {
		if target.Status != operationrun.OperationRunTargetStatusBlocked {
			continue
		}

		state := decodeConflictRetryState(target.RetryState)
		if state.BlockedSince.IsZero() {
			continue
		}

		if now.Sub(state.BlockedSince) >= policy.RetryTimeout {
			return true, fmt.Sprintf(
				"target %s blocked by rack conflict for %s",
				target.ID, policy.RetryTimeout,
			)
		}
	}

	return false, ""
}
