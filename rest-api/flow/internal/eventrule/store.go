// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"context"
	"errors"
	"fmt"
	"time"
	"unicode/utf8"

	"github.com/google/uuid"
)

// ErrRuleNotFound identifies an unsuccessful rule lookup.
var ErrRuleNotFound = errors.New("event rule not found")

// ErrInvalidPersistedRule identifies persisted rule data that cannot be
// decoded into a valid domain rule. Retrying without repairing the stored data
// cannot succeed.
var ErrInvalidPersistedRule = errors.New("invalid persisted event rule")

// ErrInvalidPersistedEvent identifies persisted event data that cannot be
// decoded into a valid durable event.
var ErrInvalidPersistedEvent = errors.New("invalid persisted event")

// ErrInvalidPersistedExecution identifies persisted execution data that
// cannot be decoded into a valid domain execution.
var ErrInvalidPersistedExecution = errors.New("invalid persisted event execution")

// ErrExecutionNotFound identifies an unsuccessful execution lookup.
var ErrExecutionNotFound = errors.New("execution not found")

// ErrEventNotFound identifies an unsuccessful durable event lookup.
var ErrEventNotFound = errors.New("event not found")

// ErrExecutionAlreadyExists identifies an execution identity that existed
// before its event plan was committed.
var ErrExecutionAlreadyExists = errors.New("execution already exists")

// ErrExecutionClaimLost identifies a fenced update from a worker that no
// longer owns the execution attempt.
var ErrExecutionClaimLost = errors.New("execution claim lost")

// ErrExecutionNotClaimable identifies an execution whose current state does
// not permit acquiring a claim.
var ErrExecutionNotClaimable = errors.New("execution is not claimable")

// ErrInvalidExecutionInput identifies invalid arguments passed to an
// execution operation.
var ErrInvalidExecutionInput = errors.New("invalid execution input")

const maxExecutionClaimOwnerRunes = 128

// ValidateExecutionClaimOwner checks that a claim owner is a bounded, nonempty
// identity.
func ValidateExecutionClaimOwner(owner string) error {
	if err := validateRequiredString("execution claim owner", owner); err != nil {
		return err
	}
	if utf8.RuneCountInString(owner) > maxExecutionClaimOwnerRunes {
		return fmt.Errorf(
			"execution claim owner exceeds %d characters",
			maxExecutionClaimOwnerRunes,
		)
	}

	return nil
}

// ExecutionClaimRequest bounds one atomic scheduler-store selection.
type ExecutionClaimRequest struct {
	Owner         string
	Limit         int
	ClaimDuration time.Duration
	MaxAttempts   int
}

// Validate checks the owner, batch limit, claim duration, and attempt bound.
func (r ExecutionClaimRequest) Validate() error {
	if err := ValidateExecutionClaimOwner(r.Owner); err != nil {
		return invalidExecutionInputErrorf("%w", err)
	}
	if r.Limit <= 0 {
		return invalidExecutionInputErrorf("execution claim limit must be positive")
	}
	if r.ClaimDuration <= 0 {
		return invalidExecutionInputErrorf("execution claim duration must be positive")
	}
	if r.MaxAttempts <= 0 {
		return invalidExecutionInputErrorf("execution claim max attempts must be positive")
	}

	return nil
}

// ClaimedExecution contains one running execution and its ownership fence.
// Token is not a downstream idempotency key.
type ClaimedExecution struct {
	Execution Execution
	Token     uuid.UUID
}

// ExecutionClaimBatch contains claims produced by one bounded candidate scan.
// ScanLimitReached means the store inspected request.Limit eligible rows, so
// additional work may remain even when exhausted rows reduced Claims.
type ExecutionClaimBatch struct {
	Claims           []ClaimedExecution
	ScanLimitReached bool
}

// Validate checks the running execution and ownership fence.
func (c ClaimedExecution) Validate() error {
	if err := c.Execution.Validate(); err != nil {
		return fmt.Errorf("execution: %w", err)
	}

	if c.Execution.Status != ExecutionStatusRunning {
		return fmt.Errorf(
			"claimed execution %s has status %q, want %q",
			c.Execution.ID,
			c.Execution.Status,
			ExecutionStatusRunning,
		)
	}
	if c.Token == uuid.Nil {
		return fmt.Errorf("execution claim token is required")
	}
	if c.Execution.ClaimToken != c.Token {
		return fmt.Errorf("execution claim token does not match running execution")
	}

	return nil
}

// RuleFilter limits rules returned by a store.
type RuleFilter struct {
	EventType *Type
	Enabled   *bool
}

// RuleListRequest applies filters before selecting one deterministic page.
type RuleListRequest struct {
	Filter RuleFilter
	Offset int
	Limit  int
}

// Validate checks the offset-pagination contract.
func (r RuleListRequest) Validate() error {
	if r.Offset < 0 {
		return fmt.Errorf("event rule list offset must not be negative")
	}
	if r.Limit <= 0 {
		return fmt.Errorf("event rule list limit must be positive")
	}

	return nil
}

// RuleListPage contains one page and the total number of matching rules before
// pagination.
type RuleListPage struct {
	Rules []*Rule
	Total int
}

// Matches reports whether a rule satisfies every configured filter field.
func (f RuleFilter) Matches(rule *Rule) bool {
	if rule == nil {
		return false
	}

	if f.EventType != nil && rule.EventType != *f.EventType {
		return false
	}
	if f.Enabled != nil && rule.Enabled != *f.Enabled {
		return false
	}

	return true
}

// RuleReader is the common read capability for built-in and persisted rules.
type RuleReader interface {
	GetByID(context.Context, uuid.UUID) (*Rule, error)
	List(context.Context, RuleListRequest) (RuleListPage, error)
}

// BuiltInRuleReader adds unique event-type lookup for built-in rules.
type BuiltInRuleReader interface {
	RuleReader
	GetByEventType(context.Context, Type) (*Rule, error)
}

// RuleStore manages persisted rule lifecycle operations. Callers must validate
// domain values before passing them to mutation methods. Implementations remain
// responsible for enforcing aggregate and persistence invariants before writes.
// Mutations targeting an unknown rule ID must return ErrRuleNotFound.
type RuleStore interface {
	RuleReader
	// Create persists a manager-constructed rule and returns the stored rule,
	// including any persistence-generated timestamps.
	Create(context.Context, *Rule) (*Rule, error)
	UpdateMetadata(context.Context, uuid.UUID, RuleMetadata) error
	ReplaceActions(context.Context, uuid.UUID, []Action) error
	// Delete atomically deletes a persisted rule and all of its bindings.
	// Implementations own the transaction that enforces this invariant.
	Delete(context.Context, uuid.UUID) error
	// SetEnabled atomically applies the desired state. Repeating the existing
	// state succeeds without changing UpdatedAt.
	SetEnabled(context.Context, uuid.UUID, bool) error
}

// BindingStore manages persisted rule bindings and scope lookup.
type BindingStore interface {
	Bind(context.Context, Binding) error
	// Unbind atomically removes the binding for an event type and scope. It
	// returns ErrBindingNotFound when that resolution slot has no binding.
	Unbind(context.Context, Type, Scope) error
	// GetForScope returns the binding for an event type and scope. When no
	// binding exists, implementations must return (nil, nil).
	GetForScope(context.Context, Type, Scope) (*Binding, error)
}

// EventPlanStore owns the source-event duplicate fast path and atomic event-plan
// commit. Implementations own all persistence timestamps.
type EventPlanStore interface {
	// ObserveEvent records an existing event observation and returns (nil, nil)
	// when no event is persisted.
	ObserveEvent(context.Context, EventKey) (*Event, error)
	// CommitEventPlan is all-or-nothing: it persists the event and one execution
	// per planned action in the same transaction. A concurrent duplicate records
	// another observation and returns (nil, nil). Any error persists nothing.
	CommitEventPlan(
		ctx context.Context,
		event Event,
		planned []PlannedExecution,
	) (*Event, error)
}

// ExecutionStore owns scheduler claims and fenced attempt outcomes.
// Implementations own all persistence and retry timestamps.
type ExecutionStore interface {
	// ClaimPendingExecutions atomically selects at most request.Limit pending
	// rows, moves them to running, allocates attempts, and assigns request.Owner
	// with fencing tokens and store-timed expirations.
	ClaimPendingExecutions(
		ctx context.Context,
		request ExecutionClaimRequest,
	) (ExecutionClaimBatch, error)
	// ClaimRetryExecutions atomically scans at most request.Limit due deferred or
	// expired running rows. Expired rows whose attempt budget is exhausted are
	// failed and may reduce the returned claim count; ScanLimitReached tells the
	// caller to schedule another bounded scan.
	ClaimRetryExecutions(
		ctx context.Context,
		request ExecutionClaimRequest,
	) (ExecutionClaimBatch, error)
	// TransitionClaimedExecution persists an attempt outcome only while token
	// owns the running execution. Expiration permits reclamation but does not
	// invalidate the token before a reclaimer replaces it.
	TransitionClaimedExecution(
		ctx context.Context,
		executionID uuid.UUID,
		token uuid.UUID,
		result ExecutionResult,
	) error
}

// Store is the complete persistence boundary required by the event-rule
// manager.
type Store interface {
	RuleStore
	BindingStore
	EventPlanStore
	ExecutionStore
}
