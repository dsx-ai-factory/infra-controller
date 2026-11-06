// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"

	"go.temporal.io/api/enums/v1"
	temporalclient "go.temporal.io/sdk/client"

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
		return nil, fmt.Errorf("failed to describe workflow execution: %w", err)
	}
	info := resp.GetWorkflowExecutionInfo()
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
