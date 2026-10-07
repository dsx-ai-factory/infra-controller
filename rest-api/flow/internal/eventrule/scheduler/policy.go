// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"errors"
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
)

// PolicyConfig contains scheduler-owned execution policies.
type PolicyConfig struct {
	// MaxAttempts limits attempts that return retryable execution failures.
	// Interrupted attempts are refunded when their result is persisted.
	MaxAttempts  int
	InitialDelay time.Duration
	MaxDelay     time.Duration
}

// DefaultPolicyConfig returns the default scheduler policy configuration.
func DefaultPolicyConfig() PolicyConfig {
	return PolicyConfig{
		MaxAttempts:  4,
		InitialDelay: 10 * time.Second,
		MaxDelay:     time.Minute,
	}
}

// Validate checks every scheduler policy.
func (p PolicyConfig) Validate() error {
	if p.MaxAttempts <= 0 {
		return fmt.Errorf("retry max attempts must be positive")
	}

	if p.InitialDelay <= 0 {
		return fmt.Errorf("retry initial delay must be positive")
	}

	if p.MaxDelay < p.InitialDelay {
		return fmt.Errorf("retry max delay must be at least the initial delay")
	}

	return nil
}

func (p PolicyConfig) resultForExecutionError(
	attempts int,
	err error,
) eventrule.ExecutionResult {
	if executor.IsInterrupted(err) {
		return eventrule.DeferredExecutionResult(
			eventrule.ExecutionReasonAttemptInterrupted,
			err.Error(),
			p.retryDelay(attempts),
		)
	}

	if errors.Is(err, executor.ErrRetryable) {
		return p.resultForRetryableError(
			attempts,
			eventrule.ExecutionReasonAttemptFailed,
			err,
		)
	}

	return eventrule.FailedExecutionResult(err.Error())
}

func (p PolicyConfig) resultForRetryableError(
	attempts int,
	reason eventrule.ExecutionReason,
	err error,
) eventrule.ExecutionResult {
	if attempts < p.MaxAttempts {
		return eventrule.DeferredExecutionResult(
			reason,
			err.Error(),
			p.retryDelay(attempts),
		)
	}

	err = fmt.Errorf("execution failed after %d attempts: %w", attempts, err)

	return eventrule.FailedExecutionResult(err.Error())
}

// retryDelay returns exponential backoff for a failed allocated attempt.
func (p PolicyConfig) retryDelay(attempts int) time.Duration {
	delay := p.InitialDelay
	for attempt := 1; attempt < attempts; attempt++ {
		if delay >= p.MaxDelay/2 {
			return p.MaxDelay
		}

		delay *= 2
	}

	if delay > p.MaxDelay {
		return p.MaxDelay
	}

	return delay
}
