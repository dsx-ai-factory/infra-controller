// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"fmt"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/common"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/task"
)

// Executor is the engine-agnostic interface for executing tasks. Implementations
// hide all engine-specific details (Temporal, local, mock) from the task manager.
type Executor interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error
	Type() common.ExecutorType
	// Execute dispatches a task to the appropriate handler based on
	// req.Info.OperationType. The operation payload must be pre-serialized
	// into req.Info.OperationInfo before calling.
	Execute(ctx context.Context, req *task.ExecutionRequest) (*task.ExecutionResponse, error)
	CheckStatus(ctx context.Context, executionID string) (common.TaskStatus, error)
	TerminateTask(ctx context.Context, executionID string, reason string) error
}

// ExecutorConfig is implemented by engine-specific configuration structs.
// Build is called once at startup to construct the live Executor.
type ExecutorConfig interface {
	Validate() error
	// Build constructs the Executor. updater receives task status transitions
	// from the execution engine back to the store and must not be nil.
	// reportUpdater receives in-flight report snapshots and may be nil; when
	// nil, activities that need it return an error at invocation time.
	Build(
		ctx context.Context,
		updater task.TaskStatusUpdater,
		reportUpdater task.TaskReportUpdater,
	) (Executor, error)
}

// New validates the config and builds the Executor, wiring updater so the
// engine can report task status changes without importing store packages.
// reportUpdater is plumbed through the same call to persist progress
// snapshots; nil is allowed and disables in-flight reporting.
func New(
	ctx context.Context,
	executorConfig ExecutorConfig,
	updater task.TaskStatusUpdater,
	reportUpdater task.TaskReportUpdater,
) (Executor, error) {
	if executorConfig == nil {
		return nil, fmt.Errorf("executor config is required")
	}

	if err := executorConfig.Validate(); err != nil {
		return nil, err
	}

	if updater == nil {
		return nil, fmt.Errorf("task status updater is required")
	}

	return executorConfig.Build(ctx, updater, reportUpdater)
}
