// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Task defines the details of a task. It includes:
// -- ID: The unique identifier of the task.
// -- Operation: The operation to be performed by the task.
// -- RackID: The rack this task operates on (1 task = 1 rack).
// -- Attributes: Flexible metadata including targeted components by type.
// -- Description: User-provided description at task creation.
// -- ExecutorType: The type of executor to be used for the task.
// -- ExecutionID: The identifier of the execution of the task.
// -- Status: The status of the task.
// -- Message: Brief text tied to status (not execution progress).
// -- Report: Structured JSON progress document (task.report).
// -- AppliedRuleID: The ID of the operation rule that was applied (if any).
type Task struct {
	ID            uuid.UUID
	Operation     operation.Wrapper
	RackID        uuid.UUID // The rack this task operates on (1 task = 1 rack)
	Attributes    taskcommon.TaskAttributes
	Description   string
	ExecutorType  taskcommon.ExecutorType
	ExecutionID   string
	Status        taskcommon.TaskStatus
	Message       string
	Report        json.RawMessage
	AppliedRuleID *uuid.UUID // The ID of the operation rule that was applied
	CreatedAt     time.Time
	UpdatedAt     time.Time
	StartedAt     *time.Time
	FinishedAt    *time.Time

	// QueueExpiresAt is the deadline for a pre-execution wait. After this time
	// the Promoter or task manager terminates the task automatically.
	// Nil for non-waiting tasks.
	QueueExpiresAt *time.Time

	// IdempotencyKey is an optional stable submission key. When set, retries
	// return the existing task instead of creating another row.
	IdempotencyKey string
	TriggerType    operation.TriggerType
	TriggerID      *uuid.UUID
}

// IsScheduled reports whether the task has been submitted to an executor.
func (t *Task) IsScheduled() bool {
	return t != nil && t.ExecutionID != ""
}

// WorkflowComponent holds the minimal component data needed to execute
// a workflow. All fields are plain JSON-safe types.
type WorkflowComponent struct {
	Type        devicetypes.ComponentType `json:"type"`
	ComponentID string                    `json:"component_id"`
}

// ExecutionInfo contains the information needed to execute a task.
type ExecutionInfo struct {
	TaskID     uuid.UUID
	Components []WorkflowComponent

	// RuleDefinition is the resolved operation rule, determined at task
	// creation time and carried through to the workflow unchanged.
	RuleDefinition *operationrules.RuleDefinition

	// OperationType identifies which workflow to dispatch to. The executor
	// looks up the registered WorkflowDescriptor by this value and submits
	// the workflow by its stable Temporal name.
	OperationType taskcommon.TaskType

	// OperationInfo is the serialized operation-specific payload. The
	// executor passes it to the WorkflowDescriptor's Unmarshal function,
	// which deserializes and validates it before the workflow starts.
	OperationInfo json.RawMessage
}

// ExecutionRequest holds the parameters for submitting a task for execution.
type ExecutionRequest struct {
	Info  ExecutionInfo
	Async bool
}

// ExecutionResponse holds the result of a task execution submission.
type ExecutionResponse struct {
	ExecutionID string
}

// Validate returns an error if the ExecutionRequest is missing required fields.
func (r *ExecutionRequest) Validate() error {
	if r == nil {
		return fmt.Errorf("request is nil")
	}

	if r.Info.TaskID == uuid.Nil {
		return fmt.Errorf("task ID is nil")
	}

	if !r.Info.OperationType.IsValid() {
		return fmt.Errorf("operation type is invalid or not set")
	}

	if len(r.Info.OperationInfo) == 0 {
		return fmt.Errorf("operation info is empty")
	}

	if !json.Valid(r.Info.OperationInfo) {
		return fmt.Errorf("operation info is not valid JSON")
	}

	if len(r.Info.Components) == 0 {
		return fmt.Errorf("components list is empty")
	}

	return nil
}

// IsValid reports whether the ExecutionResponse contains a non-empty execution ID.
func (r *ExecutionResponse) IsValid() bool {
	if r == nil {
		return false
	}

	if r.ExecutionID == "" {
		return false
	}

	return true
}

// TaskStatusUpdate carries the fields needed to update a task's status.
type TaskStatusUpdate struct {
	ID      uuid.UUID
	Status  taskcommon.TaskStatus
	Message string
	// QueueExpiresAt, when non-nil, replaces the task's pre-execution wait
	// deadline. A nil value leaves it unchanged unless Status is finished, in
	// which case the stored deadline is cleared.
	QueueExpiresAt *time.Time
	// Report, when non-empty, replaces the stored report document. An
	// empty value leaves the stored report untouched.
	Report json.RawMessage
}

// TaskReportUpdate replaces the stored report with the supplied snapshot
// without changing status or message. Empty snapshots are dropped to
// avoid clearing the stored report by accident.
type TaskReportUpdate struct {
	ID     uuid.UUID
	Report json.RawMessage
}

// TaskStatusUpdater is implemented by any store that can persist task status changes.
type TaskStatusUpdater interface {
	// UpdateTaskStatus persists the status change described by arg.
	UpdateTaskStatus(ctx context.Context, arg *TaskStatusUpdate) error
}

// TaskReportUpdater persists in-flight report snapshots (best-effort).
type TaskReportUpdater interface {
	UpdateTaskReport(ctx context.Context, arg *TaskReportUpdate) error
}
