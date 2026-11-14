// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpc

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	commonpb "go.temporal.io/api/common/v1"
	enumspb "go.temporal.io/api/enums/v1"
	failurepb "go.temporal.io/api/failure/v1"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/api/serviceerror"
	workflowpb "go.temporal.io/api/workflow/v1"
	workflowservice "go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestObserveCreation(t *testing.T) {
	cases := []struct {
		name                string
		status              enumspb.WorkflowExecutionStatus
		describeErr         error
		rejectionType       string
		attempt             int32
		created             bool
		rejected            bool
		wantErr             bool
		laterRun            bool
		wrongActivity       bool
		historyError        bool
		wrongDescribeID     bool
		wrongFailedActivity bool
	}{
		{name: "running execution is observed without waiting", status: enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING},
		{name: "completed original execution returns its typed result", status: enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED, created: true},
		{name: "another described workflow is not the original result", status: enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED, wrongDescribeID: true},
		{name: "missing history remains unknown", describeErr: serviceerror.NewNotFound("history expired"), wantErr: true},
		{name: "timeout is not a rejection", status: enumspb.WORKFLOW_EXECUTION_STATUS_TIMED_OUT},
		{name: "original precommit rejection is terminal", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoInvalidArgument, attempt: 1, rejected: true},
		{name: "later activity rejection cannot disprove an earlier commit", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoInvalidArgument, attempt: 2},
		{name: "Core unavailable remains ambiguous", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoUnavailable, wantErr: true},
		{name: "AlreadyExists can reflect an earlier commit", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoAlreadyExists, wantErr: true},
		{name: "later workflow run cannot prove original rejection", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoInvalidArgument, attempt: 1, laterRun: true},
		{name: "another activity cannot prove original rejection", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoInvalidArgument, attempt: 1, wrongActivity: true},
		{name: "failure from another activity type is not creation rejection", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoInvalidArgument, wrongFailedActivity: true, wantErr: true},
		{name: "interrupted history remains unknown", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, rejectionType: swe.ErrTypeNICoInvalidArgument, attempt: 1, historyError: true, wantErr: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			const workflowID, runID = "vpc-create-original", "original-run"
			stc := &tmocks.Client{}
			described := &workflowservice.DescribeWorkflowExecutionResponse{
				WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{
					Type: &commonpb.WorkflowType{Name: "CreateVPCV2"}, Status: tc.status,
					Execution: &commonpb.WorkflowExecution{WorkflowId: workflowID, RunId: runID},
				},
			}
			if tc.wrongDescribeID {
				described.WorkflowExecutionInfo.Execution.WorkflowId = "another-workflow"
			}
			stc.On("DescribeWorkflowExecution", mock.Anything, workflowID, "").Return(described, tc.describeErr).Once()
			vni := uint32(50001)
			expected := &corev1.Vpc{Id: &corev1.VpcId{Value: "original-vpc"},
				Config:   &corev1.VpcConfig{TenantOrganizationId: "original-tenant"},
				Metadata: &corev1.Metadata{Name: "original metadata"}, Status: &corev1.VpcStatus{Vni: &vni}}
			if !tc.wrongDescribeID && (tc.status == enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED || tc.status == enumspb.WORKFLOW_EXECUTION_STATUS_FAILED) {
				run := &tmocks.WorkflowRun{}
				t.Cleanup(func() { run.AssertExpectations(t) })
				stc.On("GetWorkflow", mock.Anything, workflowID, runID).Return(run).Once()
				var resultErr error
				if tc.rejectionType != "" {
					converter := temporal.GetDefaultFailureConverter()
					activityType := "CreateVpcOnSite"
					if tc.wrongFailedActivity {
						activityType = "UpdateVpcOnSite"
					}
					resultErr = converter.FailureToError(&failurepb.Failure{
						Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("Core rejected create", tc.rejectionType, nil)),
						FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{
							ActivityType: &commonpb.ActivityType{Name: activityType}, ActivityId: "create", ScheduledEventId: 2, StartedEventId: 3,
						}},
					})
				}
				run.On("GetWithOptions", mock.Anything, mock.Anything, client.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
					proto.Merge(args.Get(1).(*corev1.Vpc), expected)
				}).Return(resultErr).Once()
				if tc.attempt != 0 {
					events := []*historypb.HistoryEvent{
						{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{
							WorkflowType: &commonpb.WorkflowType{Name: "CreateVPCV2"}, Attempt: 1, OriginalExecutionRunId: runID, FirstExecutionRunId: runID,
						}}},
						{EventId: 2, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{
							ActivityType: &commonpb.ActivityType{Name: "CreateVpcOnSite"}, ActivityId: "create",
						}}},
						{EventId: 3, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{
							ScheduledEventId: 2, Attempt: tc.attempt,
						}}},
					}
					history := &tmocks.HistoryEventIterator{}
					t.Cleanup(func() { history.AssertExpectations(t) })
					if tc.laterRun {
						events[0].GetWorkflowExecutionStartedEventAttributes().OriginalExecutionRunId = "earlier-run"
						events = events[:1]
					}
					if tc.wrongActivity {
						events[1].GetActivityTaskScheduledEventAttributes().ActivityId = "another-activity"
					}
					if tc.historyError {
						events = events[:1]
					}
					for _, event := range events {
						history.On("HasNext").Return(true).Once()
						history.On("Next").Return(event, nil).Once()
					}
					if tc.historyError {
						history.On("HasNext").Return(true).Once()
						history.On("Next").Return(nil, errors.New("history read failed")).Once()
					} else if !tc.rejected && !tc.laterRun {
						history.On("HasNext").Return(false).Once()
					}
					stc.On("GetWorkflowHistory", mock.Anything, workflowID, runID, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(history).Once()
				}
			}
			created, rejected, err := ObserveCreation(context.Background(), stc, workflowID)
			assert.Equal(t, tc.created, created != nil)
			if created != nil {
				assert.True(t, proto.Equal(expected, created))
			}
			assert.Equal(t, tc.rejected, rejected)
			if tc.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
			stc.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			stc.AssertExpectations(t)
		})
	}
}
