// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// TaskManager is the downstream task-submission surface used by TaskExecutor.
type TaskManager interface {
	SubmitTask(context.Context, *operation.Request) ([]uuid.UUID, error)
}

// TaskExecutor submits one idempotent task per persisted rack target.
type TaskExecutor struct {
	manager TaskManager
}

// Execute submits every rack target from the immutable plan. The task manager
// returns the existing task when a retry repeats an idempotency key.
func (e *TaskExecutor) Execute(ctx context.Context, request ExecutionRequest) error {
	if e == nil || e.manager == nil {
		return terminalError(fmt.Errorf("task manager is required"))
	}

	plan, ok := request.Plan.(*eventrule.SubmitTaskPlan)
	if !ok || plan == nil {
		return terminalError(fmt.Errorf(
			"task executor received plan %T",
			request.Plan,
		))
	}

	for _, target := range plan.Targets {
		if err := e.submitTarget(ctx, request.ExecutionID, plan, target); err != nil {
			return err
		}
	}

	return nil
}

func (e *TaskExecutor) submitTarget(
	ctx context.Context,
	executionID uuid.UUID,
	plan *eventrule.SubmitTaskPlan,
	target operation.RackExecutionTarget,
) error {
	request, err := operationRequest(executionID, plan, target)
	if err != nil {
		return terminalError(err)
	}

	taskIDs, err := e.manager.SubmitTask(ctx, request)
	if err != nil {
		return retryableError("submit task", err)
	}

	if len(taskIDs) != 1 || taskIDs[0] == uuid.Nil {
		return terminalError(errors.New("task submission did not return exactly one valid task id"))
	}

	return nil
}

func operationRequest(
	executionID uuid.UUID,
	plan *eventrule.SubmitTaskPlan,
	target operation.RackExecutionTarget,
) (*operation.Request, error) {
	componentIDs := target.ComponentsByType.AllComponentUUIDs()
	componentTargets := make([]operation.ComponentTarget, len(componentIDs))
	for i, id := range componentIDs {
		componentTargets[i] = operation.ComponentTarget{UUID: id}
	}

	request := &operation.Request{
		Operation:        plan.Operation,
		TargetSpec:       operation.TargetSpec{Components: componentTargets},
		Description:      plan.Description,
		ConflictStrategy: plan.ConflictStrategy,
		RuleID:           operations.ExtractRuleID(plan.Operation.Info),
		RequiredRackID:   target.RackID,
		TriggerType:      operation.TriggerTypeEventRuleExecution,
		TriggerID:        &executionID,
		IdempotencyKey:   taskIdempotencyKey(executionID, target.RackID),
	}

	if err := request.Validate(); err != nil {
		return nil, fmt.Errorf("validate task request: %w", err)
	}

	return request, nil
}

func taskIdempotencyKey(executionID uuid.UUID, rackID uuid.UUID) string {
	return fmt.Sprintf("event-rule-execution:%s:rack:%s", executionID, rackID)
}

var _ Executor = (*TaskExecutor)(nil)
