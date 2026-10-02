// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"fmt"
	"time"

	"github.com/google/uuid"
)

// ExecutionKey identifies one action execution within a durable event.
type ExecutionKey struct {
	EventID    uuid.UUID
	ActionName string
}

// Validate checks the execution planning identity.
func (k ExecutionKey) Validate() error {
	if k.EventID == uuid.Nil {
		return fmt.Errorf("execution event id is required")
	}

	return ValidateIdentifier("event rule action name", k.ActionName)
}

// Execution records one immutable action plan and its mutable processing
// state.
type Execution struct {
	ExecutionState
	ID         uuid.UUID
	EventID    uuid.UUID
	ActionName string
	Plan       ExecutionPlan
	// Attempts counts attempts that consume the retry budget. Claiming an
	// execution allocates an attempt; an interrupted attempt is refunded when
	// the execution is deferred.
	Attempts   int
	ClaimToken uuid.UUID
	ClaimOwner string
	// ClaimExpiresAt is the store-timed expiration of the active running claim.
	ClaimExpiresAt time.Time
	CreatedAt      time.Time
	UpdatedAt      time.Time
}

// Clone returns an independent execution snapshot.
func (e Execution) Clone() Execution {
	cloned := e
	cloned.Plan = CloneExecutionPlan(e.Plan)

	return cloned
}

func (e *Execution) validateNow(now time.Time) error {
	if now.IsZero() {
		return fmt.Errorf("execution time is required")
	}
	if now.Before(e.CreatedAt) {
		return fmt.Errorf("execution time cannot precede creation time")
	}
	if now.Before(e.UpdatedAt) {
		return fmt.Errorf("execution time cannot precede update time")
	}

	return nil
}

func (e *Execution) claimExpiredAt(t time.Time) bool {
	return !t.Before(e.ClaimExpiresAt)
}

func invalidExecutionInputErrorf(format string, args ...any) error {
	return fmt.Errorf("%w: %w", ErrInvalidExecutionInput, fmt.Errorf(format, args...))
}

func executionNotClaimableErrorf(format string, args ...any) error {
	return fmt.Errorf("%w: %w", ErrExecutionNotClaimable, fmt.Errorf(format, args...))
}

func executionClaimLostErrorf(format string, args ...any) error {
	return fmt.Errorf("%w: %w", ErrExecutionClaimLost, fmt.Errorf(format, args...))
}

// ClaimDisposition describes the durable result of acquiring an execution
// claim.
type ClaimDisposition uint8

const (
	// ClaimUnspecified means claim acquisition did not produce a disposition.
	ClaimUnspecified ClaimDisposition = iota
	// ClaimAcquired means ownership was assigned and the execution is running.
	ClaimAcquired
	// ClaimExhausted means an expired running execution reached its attempt
	// limit and was failed instead of assigning new ownership.
	ClaimExhausted
)

// AcquireClaim assigns the next attempt to an eligible execution or fails an
// expired running execution whose attempt limit is exhausted. Invalid
// arguments return ErrInvalidExecutionInput; an ineligible state returns
// ErrExecutionNotClaimable.
func (e *Execution) AcquireClaim(
	owner string,
	token uuid.UUID,
	now time.Time,
	claimExpiresAt time.Time,
	maxAttempts int,
) (ClaimDisposition, error) {
	if maxAttempts <= 0 {
		return ClaimUnspecified, invalidExecutionInputErrorf(
			"execution claim max attempts must be positive",
		)
	}
	if err := ValidateExecutionClaimOwner(owner); err != nil {
		return ClaimUnspecified, invalidExecutionInputErrorf("%w", err)
	}
	if token == uuid.Nil {
		return ClaimUnspecified, invalidExecutionInputErrorf(
			"execution claim token is required",
		)
	}
	if err := e.validateNow(now); err != nil {
		return ClaimUnspecified, invalidExecutionInputErrorf("%w", err)
	}
	if !claimExpiresAt.After(now) {
		return ClaimUnspecified, invalidExecutionInputErrorf(
			"execution claim expiration must follow claim time",
		)
	}

	if !e.Status.CanBeClaimed() {
		return ClaimUnspecified, executionNotClaimableErrorf(
			"execution %s cannot acquire claim from %q",
			e.ID,
			e.Status,
		)
	}

	if e.Status == ExecutionStatusRunning {
		if !e.claimExpiredAt(now) {
			return ClaimUnspecified, executionNotClaimableErrorf(
				"execution %s does not have an expired claim",
				e.ID,
			)
		}

		if e.Attempts >= maxAttempts {
			e.ExecutionState = FailedExecutionResult(
				fmt.Sprintf("execution claim expired after %d attempts", e.Attempts),
			).stateAt(now)
			e.ClaimToken = uuid.Nil
			e.ClaimOwner = ""
			e.ClaimExpiresAt = time.Time{}
			e.UpdatedAt = now

			return ClaimExhausted, nil
		}

		// The original claim may have expired because the configured duration
		// underestimated a valid execution rather than because its worker died.
		// Give every replacement attempt twice the configured duration so it has
		// another bounded opportunity to finish without allowing recovery windows
		// to grow exponentially across repeated reclamations.
		claimExpiresAt = claimExpiresAt.Add(claimExpiresAt.Sub(now))
	}

	e.ExecutionState = ExecutionState{
		ExecutionStatusDetails: ExecutionStatusDetails{
			Status: ExecutionStatusRunning,
		},
	}
	e.Attempts++
	e.ClaimToken = token
	e.ClaimOwner = owner
	e.ClaimExpiresAt = claimExpiresAt
	e.UpdatedAt = now

	return ClaimAcquired, nil
}

// TransitionClaimedTo validates and applies the active attempt's result when
// token still owns the execution, including after its fixed expiration when it
// has not yet been reclaimed. Invalid arguments return ErrInvalidExecutionInput;
// lost ownership returns ErrExecutionClaimLost.
func (e *Execution) TransitionClaimedTo(
	token uuid.UUID,
	result ExecutionResult,
	now time.Time,
) error {
	if token == uuid.Nil {
		return invalidExecutionInputErrorf(
			"execution claim token is required",
		)
	}
	if err := result.Validate(); err != nil {
		return invalidExecutionInputErrorf("%w", err)
	}
	if err := e.validateNow(now); err != nil {
		return invalidExecutionInputErrorf("%w", err)
	}

	if e.Status != ExecutionStatusRunning || e.ClaimToken != token {
		return executionClaimLostErrorf("execution %s", e.ID)
	}

	if result.Reason == ExecutionReasonAttemptInterrupted {
		e.Attempts--
	}

	e.ExecutionState = result.stateAt(now)
	e.ClaimToken = uuid.Nil
	e.ClaimOwner = ""
	e.ClaimExpiresAt = time.Time{}
	e.UpdatedAt = now

	return nil
}

func (e *Execution) validateTimestamps() error {
	if e.CreatedAt.IsZero() {
		return fmt.Errorf("execution creation time is required")
	}
	if e.UpdatedAt.IsZero() {
		return fmt.Errorf("execution updated time is required")
	}
	if e.UpdatedAt.Before(e.CreatedAt) {
		return fmt.Errorf("execution updated time cannot precede creation time")
	}

	return nil
}

func (e *Execution) validateAttempts() error {
	if e.Attempts < 0 {
		return fmt.Errorf("execution attempts cannot be negative")
	}
	if (e.Status == ExecutionStatusPending || e.Status == ExecutionStatusSkipped) &&
		e.Attempts != 0 {
		return fmt.Errorf("%s execution cannot have attempts", e.Status)
	}
	if e.Status != ExecutionStatusPending &&
		e.Status != ExecutionStatusSkipped &&
		e.Attempts == 0 &&
		!(e.Status == ExecutionStatusDeferred &&
			e.Reason == ExecutionReasonAttemptInterrupted) {
		return fmt.Errorf("%s execution requires an attempt", e.Status)
	}

	return nil
}

func (e *Execution) validateClaim() error {
	if e.Status == ExecutionStatusRunning {
		if e.ClaimToken == uuid.Nil {
			return fmt.Errorf("running execution requires claim token")
		}
		if err := ValidateExecutionClaimOwner(e.ClaimOwner); err != nil {
			return err
		}
		if e.claimExpiredAt(e.UpdatedAt) {
			return fmt.Errorf("running execution claim expiration must follow update time")
		}

		return nil
	}

	if e.ClaimToken != uuid.Nil {
		return fmt.Errorf("%s execution cannot have claim token", e.Status)
	}
	if e.ClaimOwner != "" {
		return fmt.Errorf("%s execution cannot have claim owner", e.Status)
	}
	if !e.ClaimExpiresAt.IsZero() {
		return fmt.Errorf("%s execution cannot have claim expiration", e.Status)
	}

	return nil
}

// Validate checks the durable execution aggregate.
func (e *Execution) Validate() error {
	if e == nil {
		return fmt.Errorf("execution is nil")
	}

	if e.ID == uuid.Nil {
		return fmt.Errorf("execution id is required")
	}
	if err := e.Key().Validate(); err != nil {
		return err
	}
	if err := ValidateExecutionPlan(e.Plan); err != nil {
		return fmt.Errorf("execution plan: %w", err)
	}
	if err := e.ExecutionState.Validate(); err != nil {
		return err
	}
	if err := e.validateTimestamps(); err != nil {
		return err
	}
	if err := e.validateAttempts(); err != nil {
		return err
	}
	if err := e.validateClaim(); err != nil {
		return err
	}

	return nil
}

// NewExecution constructs an execution using the store-provided creation time.
// The plan determines the execution's initial status.
func NewExecution(
	eventID uuid.UUID,
	actionName string,
	plan ExecutionPlan,
	now time.Time,
) (*Execution, error) {
	key := ExecutionKey{EventID: eventID, ActionName: actionName}
	if err := key.Validate(); err != nil {
		return nil, err
	}

	if err := ValidateExecutionPlan(plan); err != nil {
		return nil, err
	}
	if now.IsZero() {
		return nil, fmt.Errorf("execution creation time is required")
	}

	return &Execution{
		ExecutionState: ExecutionState{
			ExecutionStatusDetails: plan.initialStatus(),
		},
		ID:         uuid.New(),
		EventID:    eventID,
		ActionName: actionName,
		Plan:       CloneExecutionPlan(plan),
		Attempts:   0,
		CreatedAt:  now,
		UpdatedAt:  now,
	}, nil
}

// Key returns the execution's idempotent planning identity.
func (e Execution) Key() ExecutionKey {
	return ExecutionKey{EventID: e.EventID, ActionName: e.ActionName}
}
