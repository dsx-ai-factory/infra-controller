// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/api/common/v1"
	"go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	"go.temporal.io/api/workflow/v1"
	"go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/types/known/timestamppb"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestDescribeExecution(t *testing.T) {
	closed := timestamppb.New(time.Now().Add(-time.Hour))
	for _, tc := range []struct {
		name         string
		status       enums.WorkflowExecutionStatus
		closeTime    *timestamppb.Timestamp
		lookupError  error
		latestStatus enums.WorkflowExecutionStatus
		latestRun    string
		latestError  error
		encoded      string
		want         taskcommon.TaskStatus
		wantError    string
	}{
		{name: "timeout", status: enums.WORKFLOW_EXECUTION_STATUS_TIMED_OUT, closeTime: closed, want: taskcommon.TaskStatusTerminated},
		{name: "running", status: enums.WORKFLOW_EXECUTION_STATUS_RUNNING, want: taskcommon.TaskStatusRunning},
		{name: "completed", status: enums.WORKFLOW_EXECUTION_STATUS_COMPLETED, closeTime: closed, want: taskcommon.TaskStatusCompleted},
		{name: "missing execution and history fail at observation time", lookupError: serviceerror.NewNotFound("expired history"), latestError: serviceerror.NewNotFound("no current run"), want: taskcommon.TaskStatusFailed},
		{name: "missing execution with running successor stays active", lookupError: serviceerror.NewNotFound("expired history"), latestStatus: enums.WORKFLOW_EXECUTION_STATUS_RUNNING, want: taskcommon.TaskStatusRunning},
		{name: "exact run recovered by latest lookup keeps its engine outcome", lookupError: serviceerror.NewNotFound("transient absence"), latestRun: "original-run", latestStatus: enums.WORKFLOW_EXECUTION_STATUS_COMPLETED, closeTime: closed, want: taskcommon.TaskStatusCompleted},
		{name: "closed successor does not supply the missing run outcome", lookupError: serviceerror.NewNotFound("expired history"), latestStatus: enums.WORKFLOW_EXECUTION_STATUS_COMPLETED, want: taskcommon.TaskStatusFailed},
		{name: "successor lookup failure remains retryable", lookupError: serviceerror.NewNotFound("expired history"), latestError: serviceerror.NewUnavailable("unavailable"), wantError: "check successor workflow"},
		{name: "namespace absence is not a missing execution", lookupError: serviceerror.NewNamespaceNotFound("wrong namespace"), wantError: "failed to describe workflow"},
		{name: "network failure remains retryable", lookupError: serviceerror.NewUnavailable("unavailable"), wantError: "failed to describe workflow"},
		{name: "missing close time", status: enums.WORKFLOW_EXECUTION_STATUS_TIMED_OUT, wantError: "no close time"},
		{name: "malformed identity", encoded: `{}`, wantError: "workflow ID and run ID are required"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &mocks.Client{}
			encoded := tc.encoded
			if encoded == "" {
				encoded = `{"workflow_id":"task-1","run_id":"original-run"}`
				client.On("DescribeWorkflowExecution", mock.Anything, "task-1", "original-run").Return(
					&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflow.WorkflowExecutionInfo{Status: tc.status, CloseTime: tc.closeTime}}, tc.lookupError).Once()
			}
			var missing *serviceerror.NotFound
			if errors.As(tc.lookupError, &missing) {
				client.On("DescribeWorkflowExecution", mock.Anything, "task-1", "").Return(
					&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflow.WorkflowExecutionInfo{Status: tc.latestStatus, Execution: &common.WorkflowExecution{RunId: tc.latestRun}, CloseTime: tc.closeTime}}, tc.latestError).Once()
			}
			before := time.Now().UTC()
			got, err := describeExecution(context.Background(), client, encoded)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				require.Nil(t, got)
			} else {
				require.NoError(t, err)
				require.Equal(t, tc.want, got.Status)
				if errors.As(tc.lookupError, &missing) && tc.want == taskcommon.TaskStatusFailed {
					require.NotNil(t, got.FinishedAt)
					require.False(t, got.FinishedAt.Before(before))
					require.False(t, got.FinishedAt.After(time.Now().UTC()))
					require.Contains(t, got.Message, "recovery observation time")
					require.Contains(t, got.Message, "hardware effects are unknown")
				} else if tc.closeTime != nil {
					require.Equal(t, tc.closeTime.AsTime(), *got.FinishedAt)
					require.Contains(t, got.Message, "hardware effects are not verified")
					if tc.status == enums.WORKFLOW_EXECUTION_STATUS_TIMED_OUT {
						require.Contains(t, got.Message, "timed out")
					}
				} else {
					require.Nil(t, got.FinishedAt)
				}
			}
			client.AssertExpectations(t)
		})
	}
}
