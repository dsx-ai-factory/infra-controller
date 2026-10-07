// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// ExecutionPlan is the closed set of immutable, action-specific executor
// inputs. Planning resolves rule conditions, defaults, and targets before a
// plan is persisted.
type ExecutionPlan interface {
	Type() ActionType
	clone() ExecutionPlan
	initialStatus() ExecutionStatusDetails
	validate() error
}

// PlannedExecution is the ordered planning result for one applicable action.
// ActionName identifies the policy action and ExecutionPlan contains the
// immutable input for its executor.
type PlannedExecution struct {
	ActionName    string
	ExecutionPlan ExecutionPlan
}

// Validate checks the action identity and its corresponding execution plan.
func (p PlannedExecution) Validate() error {
	if err := ValidateIdentifier("event rule action name", p.ActionName); err != nil {
		return err
	}

	return ValidateExecutionPlan(p.ExecutionPlan)
}

// SubmitTaskPlan contains all action-derived input needed to submit one task
// for each resolved rack target.
type SubmitTaskPlan struct {
	Operation        operation.Wrapper
	Description      string
	ConflictStrategy operation.ConflictStrategy
	Targets          []operation.RackExecutionTarget
}

// Type returns the submit-task executor discriminator.
func (*SubmitTaskPlan) Type() ActionType { return ActionTypeSubmitTask }

func (p *SubmitTaskPlan) clone() ExecutionPlan {
	if p == nil {
		return nil
	}

	cloned := SubmitTaskPlan{
		Operation:        p.Operation.Clone(),
		Description:      p.Description,
		ConflictStrategy: p.ConflictStrategy,
	}

	if p.Targets != nil {
		cloned.Targets = make([]operation.RackExecutionTarget, len(p.Targets))
		for i, target := range p.Targets {
			cloned.Targets[i] = operation.RackExecutionTarget{
				RackID:           target.RackID,
				ComponentsByType: target.ComponentsByType.Clone(),
			}
		}
	}

	return &cloned
}

func (p *SubmitTaskPlan) initialStatus() ExecutionStatusDetails {
	if len(p.Targets) == 0 {
		return ExecutionStatusDetails{
			Status: ExecutionStatusSkipped,
			Reason: ExecutionReasonNoTargets,
		}
	}

	return ExecutionStatusDetails{Status: ExecutionStatusPending}
}

func (p *SubmitTaskPlan) validate() error {
	if p == nil {
		return fmt.Errorf("submit-task execution plan is required")
	}

	if err := validateTaskOperation(p.Operation); err != nil {
		return err
	}

	if err := validateOptionalString("task description", p.Description); err != nil {
		return err
	}

	switch p.ConflictStrategy {
	case operation.ConflictStrategyReject, operation.ConflictStrategyQueue:
	default:
		return fmt.Errorf("unknown task conflict strategy %d", p.ConflictStrategy)
	}

	seenRacks := make(map[uuid.UUID]struct{}, len(p.Targets))
	for i, target := range p.Targets {
		if target.RackID == uuid.Nil {
			return fmt.Errorf("targets[%d].rack_id is required", i)
		}

		if _, exists := seenRacks[target.RackID]; exists {
			return fmt.Errorf("targets[%d] duplicates rack %s", i, target.RackID)
		}

		seenRacks[target.RackID] = struct{}{}

		if err := target.ComponentsByType.Validate(); err != nil {
			return fmt.Errorf("targets[%d].components_by_type: %w", i, err)
		}
	}

	return nil
}

func validateTaskOperation(wrapper operation.Wrapper) error {
	if !wrapper.Type.IsValid() {
		return fmt.Errorf("task operation type is invalid")
	}

	if err := taskcommon.OperationCode(wrapper.Code).ValidateFor(wrapper.Type); err != nil {
		return fmt.Errorf("task operation code: %w", err)
	}

	decoded, err := operations.New(wrapper.Type, wrapper.Info)
	if err != nil {
		return fmt.Errorf("task operation payload: %w", err)
	}

	if err := decoded.Validate(); err != nil {
		return fmt.Errorf("task operation payload: %w", err)
	}

	if decoded.CodeString() != wrapper.Code {
		return fmt.Errorf(
			"task operation code %q does not match payload code %q",
			wrapper.Code,
			decoded.CodeString(),
		)
	}

	return nil
}

// SendAlertPlan contains one immutable alert submission.
type SendAlertPlan struct {
	Severity Severity
	Message  string
}

// Type returns the alert executor discriminator.
func (*SendAlertPlan) Type() ActionType { return ActionTypeSendAlert }

func (p *SendAlertPlan) clone() ExecutionPlan {
	if p == nil {
		return nil
	}

	return &SendAlertPlan{
		Severity: p.Severity,
		Message:  p.Message,
	}
}

func (*SendAlertPlan) initialStatus() ExecutionStatusDetails {
	return ExecutionStatusDetails{Status: ExecutionStatusPending}
}

func (p *SendAlertPlan) validate() error {
	if p == nil {
		return fmt.Errorf("send-alert execution plan is required")
	}

	if err := p.Severity.Validate(); err != nil {
		return err
	}

	if p.Severity.IsUnspecified() {
		return fmt.Errorf("alert severity cannot be unspecified")
	}

	return validateOptionalString("alert message", p.Message)
}

// NoopPlan contains one immutable no-op execution.
type NoopPlan struct {
	Reason string
}

// Type returns the no-op executor discriminator.
func (*NoopPlan) Type() ActionType { return ActionTypeNoop }

func (p *NoopPlan) clone() ExecutionPlan {
	if p == nil {
		return nil
	}

	return &NoopPlan{
		Reason: p.Reason,
	}
}

func (*NoopPlan) initialStatus() ExecutionStatusDetails {
	return ExecutionStatusDetails{Status: ExecutionStatusPending}
}

func (p *NoopPlan) validate() error {
	if p == nil {
		return fmt.Errorf("no-op execution plan is required")
	}

	return validateOptionalString("noop reason", p.Reason)
}

// CloneExecutionPlan returns an independent plan snapshot.
func CloneExecutionPlan(plan ExecutionPlan) ExecutionPlan {
	if plan == nil {
		return nil
	}

	return plan.clone()
}

// ValidateExecutionPlan checks a typed execution plan.
func ValidateExecutionPlan(plan ExecutionPlan) error {
	if plan == nil {
		return fmt.Errorf("execution plan is required")
	}

	return plan.validate()
}

var (
	_ ExecutionPlan = (*SubmitTaskPlan)(nil)
	_ ExecutionPlan = (*SendAlertPlan)(nil)
	_ ExecutionPlan = (*NoopPlan)(nil)
)
