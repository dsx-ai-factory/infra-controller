// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package executor defines the action-execution boundary used by event workers
// and the future retry scheduler.
package executor

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// ExecutionRequest contains the immutable identity and plan needed for one
// dispatch attempt.
type ExecutionRequest struct {
	ExecutionID uuid.UUID
	Plan        eventrule.ExecutionPlan
}

// Validate checks the execution input.
func (r ExecutionRequest) Validate() error {
	if r.ExecutionID == uuid.Nil {
		return fmt.Errorf("execution id is required")
	}

	if err := eventrule.ValidateExecutionPlan(r.Plan); err != nil {
		return fmt.Errorf("execution plan: %w", err)
	}

	return nil
}

// Executor performs the side effects for one action type.
type Executor interface {
	// Execute may be called multiple times for the same ExecutionID after a
	// deferred result. Implementations that produce external side effects must
	// use ExecutionID, or stable keys derived from it for partitioned work, to
	// make repeated calls idempotent and reconcile an ambiguous prior result
	// before submitting again. Attempt numbers must not be used as downstream
	// idempotency identities. A nil error means the action completed successfully
	// from the event-rule dispatcher's point of view.
	// Implementations preserve context errors, classify retryable operational
	// errors with ErrRetryable, and classify terminal errors with ErrTerminal.
	Execute(context.Context, ExecutionRequest) error
}
