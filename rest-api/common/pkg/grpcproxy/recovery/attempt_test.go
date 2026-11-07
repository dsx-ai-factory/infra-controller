// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package recovery

import (
	"context"
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
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

type coreAttemptHistory struct {
	events []*historypb.HistoryEvent
	err    error
}

func (h *coreAttemptHistory) HasNext() bool { return len(h.events) > 0 || h.err != nil }
func (h *coreAttemptHistory) Next() (*historypb.HistoryEvent, error) {
	if h.err != nil {
		return nil, h.err
	}
	e := h.events[0]
	h.events = h.events[1:]
	return e, nil
}

func TestObserveCoreAttempt(t *testing.T) {
	cases := []struct {
		name         string
		json         string
		resultErr    error
		errorType    string
		attempt      int32
		successor    bool
		historyErr   error
		activityName string
		unbound      bool
		want         CoreAttemptResult
	}{
		{name: "original success", json: `{}`, want: CoreAttemptSucceeded},
		{name: "missing original history retains fence", resultErr: serviceerror.NewNotFound("expired")},
		{name: "malformed typed response retains fence", json: `{"unexpected":true}`},
		{name: "absent response retains fence"},
		{name: "unbound original without a durable binder stays unknown", unbound: true},
		{name: "first original Core not found confirms deletion", errorType: swe.ErrTypeNICoObjectNotFound, attempt: 1, want: CoreAttemptNotFound},
		{name: "first original rejection permits release", errorType: swe.ErrTypeNICoFailedPrecondition, attempt: 1, want: CoreAttemptRejected},
		{name: "a different activity cannot prove a Core reply", activityName: "OtherActivity", errorType: swe.ErrTypeNICoObjectNotFound},
		{name: "later activity rejection cannot exclude an earlier commit", errorType: swe.ErrTypeNICoFailedPrecondition, attempt: 2},
		{name: "existing successor keeps original fence", errorType: swe.ErrTypeNICoObjectNotFound, successor: true},
		{name: "missing failure evidence keeps fence", errorType: swe.ErrTypeNICoFailedPrecondition, attempt: 1, historyErr: serviceerror.NewNotFound("expired")},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			stc := &tmocks.Client{}
			run := &tmocks.WorkflowRun{}
			runID := "original"
			if tc.unbound {
				runID = ""
			} else {
				resultErr := tc.resultErr
				activityName := tc.activityName
				if activityName == "" {
					activityName = "InvokeCoreGRPCOnSite"
				}
				if tc.errorType != "" {
					converter := temporal.GetDefaultFailureConverter()
					resultErr = converter.FailureToError(&failurepb.Failure{Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("Core rejected", tc.errorType, nil)), FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: activityName}, ActivityId: "delete", ScheduledEventId: 5, StartedEventId: 6}}})
				}
				stc.On("GetWorkflow", ctx, "delete-workflow", "original").Return(run).Once()
				run.On("GetWithOptions", ctx, mock.Anything, client.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
					args.Get(1).(*grpcproxy.Response).ResponseJSON = []byte(tc.json)
				}).Return(resultErr).Once()
				if tc.errorType != "" && activityName == "InvokeCoreGRPCOnSite" {
					latest := "original"
					if tc.successor {
						latest = "successor"
					}
					stc.On("DescribeWorkflowExecution", ctx, "delete-workflow", "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, Execution: &commonpb.WorkflowExecution{RunId: latest}}}, nil).Once()
					if !tc.successor {
						events := []*historypb.HistoryEvent{
							{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{WorkflowType: &commonpb.WorkflowType{Name: grpcproxy.Core.WorkflowName}, OriginalExecutionRunId: "original", FirstExecutionRunId: "original", Attempt: 1}}},
							{EventId: 5, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{ActivityId: "delete", ActivityType: &commonpb.ActivityType{Name: "InvokeCoreGRPCOnSite"}}}},
							{EventId: 6, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{ScheduledEventId: 5, Attempt: tc.attempt}}},
							{EventId: 8, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED, Attributes: &historypb.HistoryEvent_WorkflowExecutionFailedEventAttributes{WorkflowExecutionFailedEventAttributes: &historypb.WorkflowExecutionFailedEventAttributes{}}},
						}
						stc.On("GetWorkflowHistory", ctx, "delete-workflow", "original", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(&coreAttemptHistory{events: events, err: tc.historyErr}).Once()
					}
				}
			}
			got, err := ObserveCoreAttempt(ctx, stc, "delete-workflow", runID, &emptypb.Empty{}, nil)
			assert.Equal(t, tc.want, got)
			if tc.want == CoreAttemptSucceeded {
				require.NoError(t, err)
			} else {
				require.Error(t, err)
			}
			stc.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			stc.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}

func TestResolveCoreAttemptRunID(t *testing.T) {
	cases := []struct {
		name                                                     string
		wrongOriginal, terminalSuccessor, missing, latestChanged bool
		wantBound                                                bool
	}{
		{name: "verified original is bound before observation", wantBound: true},
		{name: "reset latest run cannot replace original", wrongOriginal: true},
		{name: "terminal successor retains unbound fence", terminalSuccessor: true},
		{name: "expired authoritative history retains unbound fence", missing: true},
		{name: "successor appearing during verification retains fence", latestChanged: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			stc := &tmocks.Client{}
			describe := func(runID string) *workflowservice.DescribeWorkflowExecutionResponse {
				return &workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Type: &commonpb.WorkflowType{Name: grpcproxy.Core.WorkflowName}, Status: enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED, Execution: &commonpb.WorkflowExecution{RunId: runID}}}
			}
			stc.On("DescribeWorkflowExecution", ctx, "attempt", "").Return(describe("original"), nil).Once()
			original := "original"
			if tc.wrongOriginal {
				original = "earlier-run"
			}
			terminal := &historypb.WorkflowExecutionCompletedEventAttributes{}
			if tc.terminalSuccessor {
				terminal.NewExecutionRunId = "successor"
			}
			events := []*historypb.HistoryEvent{
				{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{WorkflowType: &commonpb.WorkflowType{Name: grpcproxy.Core.WorkflowName}, OriginalExecutionRunId: original, FirstExecutionRunId: "original", Attempt: 1}}},
				{EventId: 2, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_COMPLETED, Attributes: &historypb.HistoryEvent_WorkflowExecutionCompletedEventAttributes{WorkflowExecutionCompletedEventAttributes: terminal}},
			}
			var historyErr error
			if tc.missing {
				historyErr = serviceerror.NewNotFound("expired")
			}
			stc.On("GetWorkflowHistory", ctx, "attempt", "original", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(&coreAttemptHistory{events: events, err: historyErr}).Once()
			if tc.wantBound || tc.latestChanged {
				latest := "original"
				if tc.latestChanged {
					latest = "successor"
				}
				stc.On("DescribeWorkflowExecution", ctx, "attempt", "").Return(describe(latest), nil).Once()
			}
			bound := ""
			runID, err := ResolveCoreAttemptRunID(ctx, stc, "attempt", "", func(runID string) error { bound = runID; return nil })
			if tc.wantBound {
				require.NoError(t, err)
				assert.Equal(t, "original", runID)
				assert.Equal(t, runID, bound)
			} else {
				require.Error(t, err)
				assert.Empty(t, runID)
				assert.Empty(t, bound)
			}
			stc.AssertExpectations(t)
		})
	}
}
