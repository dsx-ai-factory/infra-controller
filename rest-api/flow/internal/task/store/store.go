// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package store provides the storage layer for task and operation rule management.
// It defines the Store interface for persisting and retrieving task and operation rule data.
package store

import (
	"context"

	"github.com/google/uuid"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// Store defines the interface for task and operation rule data persistence.
// It provides operations for creating, retrieving, and updating both tasks and operation rules.
type Store interface {
	// Task operations

	// RunInTransaction executes fn within a database transaction.
	// The transaction is propagated through the context so that nested store
	// calls automatically participate without extra plumbing.
	RunInTransaction(ctx context.Context, fn func(ctx context.Context) error) error

	// CreateTask creates a new task record.
	CreateTask(ctx context.Context, task *taskdef.Task) error

	// LockRack takes a transaction-scoped lock for rack-level task admission.
	// Callers must invoke it inside RunInTransaction before conflict checks.
	LockRack(ctx context.Context, rackID uuid.UUID) error

	// LockIdempotencyKey takes a transaction-scoped lock for stable submission
	// lookup and resume. Callers must invoke it inside RunInTransaction.
	LockIdempotencyKey(ctx context.Context, key string) error

	// GetTaskByIdempotencyKey retrieves the existing task for a stable
	// submission key. It returns nil when no task exists for the key.
	GetTaskByIdempotencyKey(
		ctx context.Context,
		key string,
	) (*taskdef.Task, error)

	// GetTask retrieves a single task by its ID.
	// Returns an error if the task is not found.
	GetTask(ctx context.Context, id uuid.UUID) (*taskdef.Task, error)

	// GetTasks retrieves tasks by their IDs.
	GetTasks(ctx context.Context, ids []uuid.UUID) ([]*taskdef.Task, error)

	// ListTasks lists tasks matching the given criteria.
	ListTasks(ctx context.Context, options *taskcommon.TaskListOptions, pagination *dbquery.Pagination) ([]*taskdef.Task, int32, error)

	// ListNonTerminalTasksForRacks returns Waiting, Pending, and Running tasks
	// for the requested racks.
	ListNonTerminalTasksForRacks(ctx context.Context, rackIDs []uuid.UUID) ([]*taskdef.Task, error)

	// UpdateScheduledTask updates task scheduling information (execution ID, executor type).
	UpdateScheduledTask(ctx context.Context, task *taskdef.Task) error

	// UpdateTaskStatus updates status and message, plus an optional queue deadline.
	UpdateTaskStatus(ctx context.Context, arg *taskdef.TaskStatusUpdate) error

	// UpdateTaskReport merges a report snapshot without a status change.
	UpdateTaskReport(ctx context.Context, arg *taskdef.TaskReportUpdate) error

	// ListActiveTasksForRack returns non-finished, non-waiting tasks for a rack
	// (i.e. tasks with status pending or running).
	ListActiveTasksForRack(ctx context.Context, rackID uuid.UUID) ([]*taskdef.Task, error)

	// ListWaitingTasksForRack returns waiting tasks for a rack, ordered oldest-first.
	ListWaitingTasksForRack(ctx context.Context, rackID uuid.UUID) ([]*taskdef.Task, error)

	// CountWaitingTasksForRack returns the number of waiting tasks for a rack.
	CountWaitingTasksForRack(ctx context.Context, rackID uuid.UUID) (int, error)

	// ListRacksWithWaitingTasks returns distinct rack IDs that have at least
	// one task in the waiting state.
	ListRacksWithWaitingTasks(ctx context.Context) ([]uuid.UUID, error)

	// Operation rule operations

	// CreateRule creates a new operation rule.
	CreateRule(ctx context.Context, rule *operationrules.OperationRule) error

	// UpdateRule updates specific fields of an operation rule.
	UpdateRule(ctx context.Context, id uuid.UUID, updates map[string]interface{}) error

	// DeleteRule deletes an operation rule by ID.
	DeleteRule(ctx context.Context, id uuid.UUID) error

	// SetRuleAsDefault sets a rule as the default for its operation.
	// Automatically unsets any existing default for the same (operation_type, operation).
	SetRuleAsDefault(ctx context.Context, id uuid.UUID) error

	// GetRule retrieves an operation rule by ID.
	GetRule(ctx context.Context, id uuid.UUID) (*operationrules.OperationRule, error)

	// GetRuleByName retrieves an operation rule by its name.
	GetRuleByName(ctx context.Context, name string) (*operationrules.OperationRule, error)

	// GetDefaultRule retrieves the default rule for an operation type and operation code.
	// Returns the rule with is_default=true for the given operation type and operation code.
	GetDefaultRule(ctx context.Context, opType taskcommon.TaskType, operationCode string) (*operationrules.OperationRule, error)

	// GetRuleByOperationAndRack retrieves the appropriate rule for an operation type, operation code, and rack.
	// Resolution order: rack association > default rule
	// If rackID is nil, returns the default rule.
	GetRuleByOperationAndRack(ctx context.Context, opType taskcommon.TaskType, operationCode string, rackID *uuid.UUID) (*operationrules.OperationRule, error)

	// ListRules lists operation rules matching the given criteria with pagination.
	ListRules(ctx context.Context, options *taskcommon.OperationRuleListOptions, pagination *dbquery.Pagination) ([]*operationrules.OperationRule, int32, error)

	// Rack rule association operations

	// AssociateRuleWithRack associates a rule with a rack.
	// The operation type and operation code are extracted from the rule.
	// If an association already exists, it will be updated.
	AssociateRuleWithRack(ctx context.Context, rackID uuid.UUID, ruleID uuid.UUID) error

	// DisassociateRuleFromRack removes the rule association for a rack, operation type, and operation code.
	DisassociateRuleFromRack(ctx context.Context, rackID uuid.UUID, opType taskcommon.TaskType, operationCode string) error

	// GetRackRuleAssociation retrieves the rule ID associated with a rack for an operation type and operation code.
	// Returns nil if no association exists.
	GetRackRuleAssociation(ctx context.Context, rackID uuid.UUID, opType taskcommon.TaskType, operationCode string) (*uuid.UUID, error)

	// ListRackRuleAssociations retrieves all rule associations for a rack.
	// Returns a list of associations with full details (operation type, operation code, rule ID).
	ListRackRuleAssociations(ctx context.Context, rackID uuid.UUID) ([]*operationrules.RackRuleAssociation, error)
}
