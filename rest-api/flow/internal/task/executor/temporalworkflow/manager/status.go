// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"fmt"
	"time"

	"go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	"go.temporal.io/api/workflow/v1"
	temporalclient "go.temporal.io/sdk/client"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

func describeExecution(ctx context.Context, client temporalclient.Client, encoded string) (*task.ExecutionStatus, error) {
	id, err := common.NewFromEncoded(encoded)
	if err != nil {
		return nil, err
	}
	if id.WorkflowID == "" || id.RunID == "" {
		return nil, fmt.Errorf("workflow ID and run ID are required")
	}
	resp, err := client.DescribeWorkflowExecution(ctx, id.WorkflowID, id.RunID)
	if err != nil {
		var missing *serviceerror.NotFound
		if !errors.As(err, &missing) {
			return nil, fmt.Errorf("failed to describe workflow execution: %w", err)
		}
		// Both lookups share the caller's status-check budget. A lookup failure
		// leaves the task active so another reconciliation pass can retry.
		latest, latestErr := client.DescribeWorkflowExecution(ctx, id.WorkflowID, "")
		if latestErr != nil && !errors.As(latestErr, &missing) {
			return nil, fmt.Errorf("failed to check successor workflow: %w", latestErr)
		}
		if latestErr == nil {
			info := latest.GetWorkflowExecutionInfo()
			if info == nil {
				return nil, fmt.Errorf("successor workflow has no execution info")
			}
			if info.GetExecution().GetRunId() == id.RunID {
				// A later lookup can recover the exact run after a transient
				// absence. Its known outcome takes precedence over recovery.
				return executionStatus(info)
			}
			latestStatus := taskStatusFromTemporalWorkflowStatus(info.GetStatus())
			if !latestStatus.IsFinished() {
				// A successor may still own hardware work. Do not bind its
				// outcome to the persisted run or free the rack underneath it.
				return &task.ExecutionStatus{Status: latestStatus}, nil
			}
		}
		// The engine no longer has the accepted run and no live successor was
		// found. This is a Flow recovery decision, not an engine close time.
		observed := time.Now().UTC()
		return &task.ExecutionStatus{
			Status:     taskcommon.TaskStatusFailed,
			FinishedAt: &observed,
			Message:    "Temporal execution not found; Flow marked the task failed at the recovery observation time; hardware effects are unknown; manual inspection required",
		}, nil
	}
	return executionStatus(resp.GetWorkflowExecutionInfo())
}

func executionStatus(info *workflow.WorkflowExecutionInfo) (*task.ExecutionStatus, error) {
	result := &task.ExecutionStatus{Status: taskStatusFromTemporalWorkflowStatus(info.GetStatus())}
	if result.Status.IsFinished() {
		if info.GetCloseTime() == nil {
			return nil, fmt.Errorf("closed workflow has no close time")
		}
		err := info.GetCloseTime().CheckValid()
		if err != nil {
			return nil, fmt.Errorf("invalid workflow close time: %w", err)
		}
		closed := info.GetCloseTime().AsTime()
		result.FinishedAt = &closed
		result.Message = fmt.Sprintf("Temporal workflow closed: %s; hardware effects are not verified", info.GetStatus().String())
		if info.GetStatus() == enums.WORKFLOW_EXECUTION_STATUS_TIMED_OUT {
			result.Message = "Temporal workflow timed out; hardware effects are not verified"
		}
	}
	return result, nil
}
