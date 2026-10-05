// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"fmt"

	"github.com/rs/zerolog/log"
	"go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	temporalclient "go.temporal.io/sdk/client"

	taskcommon "github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/common"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/workflow"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/internal/task/task"
)

// workflowStatusToTaskStatus maps Temporal workflow execution statuses to the
// engine-agnostic TaskStatus values exposed by the executor interface.
var (
	workflowStatusToTaskStatus = map[enums.WorkflowExecutionStatus]taskcommon.TaskStatus{ //nlint
		enums.WORKFLOW_EXECUTION_STATUS_RUNNING:          taskcommon.TaskStatusRunning,
		enums.WORKFLOW_EXECUTION_STATUS_COMPLETED:        taskcommon.TaskStatusCompleted,
		enums.WORKFLOW_EXECUTION_STATUS_FAILED:           taskcommon.TaskStatusFailed,
		enums.WORKFLOW_EXECUTION_STATUS_CANCELED:         taskcommon.TaskStatusTerminated,
		enums.WORKFLOW_EXECUTION_STATUS_TERMINATED:       taskcommon.TaskStatusTerminated,
		enums.WORKFLOW_EXECUTION_STATUS_CONTINUED_AS_NEW: taskcommon.TaskStatusRunning,
		enums.WORKFLOW_EXECUTION_STATUS_TIMED_OUT:        taskcommon.TaskStatusTerminated,
	}
)

// ignoreNotFound returns nil if err is a Temporal NotFound error, otherwise
// returns err unchanged. Use this when the absence of a workflow is an
// acceptable outcome (e.g. it already completed before the call was made).
func ignoreNotFound(err error) error {
	var notFound *serviceerror.NotFound
	if errors.As(err, &notFound) {
		return nil
	}
	return err
}

// taskStatusFromTemporalWorkflowStatus converts a Temporal workflow execution
// status to the engine-agnostic TaskStatus. Returns TaskStatusUnknown for any
// status not present in the mapping table.
func taskStatusFromTemporalWorkflowStatus(
	workflowStatus enums.WorkflowExecutionStatus,
) taskcommon.TaskStatus {
	if taskStatus, ok := workflowStatusToTaskStatus[workflowStatus]; ok {
		return taskStatus
	}
	return taskcommon.TaskStatusUnknown
}

// executeWorkflow deserializes the operation payload and submits the Temporal
// workflow described by desc. All engine-specific mechanics — client options,
// workflow ID, timeout, and the optional synchronous wait — are handled here.
func executeWorkflow(
	ctx context.Context,
	client temporalclient.Client,
	desc workflow.WorkflowDescriptor,
	req *task.ExecutionRequest,
) (*task.ExecutionResponse, error) {
	if desc.Unmarshal == nil {
		return nil, fmt.Errorf(
			"workflow %q has no unmarshal function",
			desc.WorkflowName,
		)
	}

	// Unmarshal deserializes and validates the operation payload (calls Validate()
	// on the typed info). Components are validated by req.Validate() in Execute().
	// Workflow functions therefore do not need to repeat these checks.
	typedInfo, err := desc.Unmarshal(req.Info.OperationInfo)
	if err != nil {
		return nil, fmt.Errorf(
			"failed to unmarshal operation info for %s: %w",
			req.Info.OperationType, err,
		)
	}

	r, err := client.ExecuteWorkflow(
		ctx,
		temporalclient.StartWorkflowOptions{
			TaskQueue:                WorkflowQueue,
			ID:                       req.Info.TaskID.String(),
			WorkflowExecutionTimeout: desc.Timeout,
		},
		desc.WorkflowName,
		req.Info,
		typedInfo,
	)
	if err != nil {
		return nil, fmt.Errorf("failed to execute workflow: %w", err)
	}

	executionID := &common.ExecutionID{
		WorkflowID: r.GetID(),
		RunID:      r.GetRunID(),
	}

	log.Info().Msgf("Temporal workflow %s started", executionID.String())

	encodedExecutionID, err := executionID.Encode()
	if err != nil {
		return nil, fmt.Errorf(
			"failed to encode execution ID %s: %w", executionID.String(), err,
		)
	}

	if !req.Async {
		// For synchronous requests, block until the workflow is completed.
		if err := r.Get(ctx, nil); err != nil {
			return nil, fmt.Errorf("failed to get workflow result: %w", err)
		}
	}

	return &task.ExecutionResponse{
		ExecutionID: encodedExecutionID,
	}, nil
}
