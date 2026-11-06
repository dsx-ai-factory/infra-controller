// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
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
		name        string
		status      enums.WorkflowExecutionStatus
		closeTime   *timestamppb.Timestamp
		lookupError error
		encoded     string
		want        taskcommon.TaskStatus
		wantError   string
	}{
		{name: "timeout", status: enums.WORKFLOW_EXECUTION_STATUS_TIMED_OUT, closeTime: closed, want: taskcommon.TaskStatusTerminated},
		{name: "running", status: enums.WORKFLOW_EXECUTION_STATUS_RUNNING, want: taskcommon.TaskStatusRunning},
		{name: "completed", status: enums.WORKFLOW_EXECUTION_STATUS_COMPLETED, closeTime: closed, want: taskcommon.TaskStatusCompleted},
		{name: "missing execution remains unknown", lookupError: serviceerror.NewNotFound("expired history"), wantError: "expired history"},
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
			got, err := describeExecution(context.Background(), client, encoded)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				require.Nil(t, got)
			} else {
				require.NoError(t, err)
				require.Equal(t, tc.want, got.Status)
				if tc.closeTime != nil {
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
