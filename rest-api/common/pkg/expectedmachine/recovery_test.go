// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	commonpb "go.temporal.io/api/common/v1"
	enumspb "go.temporal.io/api/enums/v1"
	failurepb "go.temporal.io/api/failure/v1"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/api/serviceerror"
	workflowpb "go.temporal.io/api/workflow/v1"
	workflowservice "go.temporal.io/api/workflowservice/v1"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestIsExpectedMachineBatchRejectionCandidate(t *testing.T) {
	cases := []struct {
		name, activity, errorType string
		cause                     error
		want                      bool
	}{
		{name: "Core rejection", activity: "CreateExpectedMachinesOnSite", errorType: swe.ErrTypeNICoInvalidArgument, want: true},
		{name: "legacy rejection", activity: "CreateExpectedMachinesOnSite", errorType: swe.ErrTypeCarbideAlreadyExists, want: true},
		{name: "request rejected before dispatch", activity: "CreateExpectedMachinesOnSite", errorType: swe.ErrTypeInvalidRequest, want: true},
		{name: "transport ambiguous", activity: "CreateExpectedMachinesOnSite", errorType: swe.ErrTypeNICoUnavailable},
		{name: "internal failure", activity: "CreateExpectedMachinesOnSite", errorType: "Internal"},
		{name: "different activity", activity: "CreateExpectedMachinesOnFlow", errorType: swe.ErrTypeNICoInvalidArgument},
		{name: "application error without activity", errorType: swe.ErrTypeNICoInvalidArgument},
		{name: "caller cancellation", cause: context.Canceled},
		{name: "execution timeout after an earlier rejection", activity: "CreateExpectedMachinesOnSite", cause: temporal.NewTimeoutError(enumspb.TIMEOUT_TYPE_START_TO_CLOSE, temporal.NewNonRetryableApplicationError("rejected", swe.ErrTypeNICoInvalidArgument, nil))},
		{name: "execution cancellation", cause: temporal.NewCanceledError()},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.cause
			if tc.errorType != "" {
				err = temporal.NewNonRetryableApplicationError("rejected", tc.errorType, nil)
			}
			if tc.activity != "" {
				converter := temporal.GetDefaultFailureConverter()
				err = converter.FailureToError(&failurepb.Failure{Message: "activity failed", Cause: converter.ErrorToFailure(err), FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: tc.activity}}}})
			}
			require.Equal(t, tc.want, IsExpectedMachineBatchRejectionCandidate(err))
		})
	}
}

func TestExpectedMachineBatchFailureCategory(t *testing.T) {
	converter := temporal.GetDefaultFailureConverter()
	rejection := converter.FailureToError(&failurepb.Failure{Message: "private activity message", Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("private rejection message", swe.ErrTypeCarbideAlreadyExists, nil)), FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}}}})
	cases := []struct {
		name string
		err  error
		want string
	}{
		{name: "missing typed response", want: "malformed_result"},
		{name: "caller deadline", err: context.DeadlineExceeded, want: "deadline_exceeded"},
		{name: "known legacy rejection has a bounded useful category", err: rejection, want: "core_already_exists"},
		{name: "untrusted application type and message stay private", err: temporal.NewNonRetryableApplicationError("private-payload", "private-peer-type", nil), want: "execution_failed"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) { require.Equal(t, tc.want, ExpectedMachineBatchFailureCategory(tc.err)) })
	}
}

type expectedMachineBatchHistory struct {
	events []*historypb.HistoryEvent
	err    error
}

func (h *expectedMachineBatchHistory) HasNext() bool { return len(h.events) > 0 || h.err != nil }
func (h *expectedMachineBatchHistory) Next() (*historypb.HistoryEvent, error) {
	if h.err != nil {
		return nil, h.err
	}
	event := h.events[0]
	h.events = h.events[1:]
	return event, nil
}

func expectedMachineBatchAttemptHistory(attempt int32) []*historypb.HistoryEvent {
	return []*historypb.HistoryEvent{
		{EventId: 10, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{ActivityId: "batch-create", ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}}}},
		{EventId: 12, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{ScheduledEventId: 10, Attempt: attempt}}},
	}
}

func TestVerifyExpectedMachineBatchRejection(t *testing.T) {
	wrongID := expectedMachineBatchAttemptHistory(1)
	wrongID[0].GetActivityTaskScheduledEventAttributes().ActivityId = "another-activity"
	wrongType := expectedMachineBatchAttemptHistory(1)
	wrongType[0].GetActivityTaskScheduledEventAttributes().ActivityType.Name = "CreateExpectedMachinesOnFlow"
	wrongLink := expectedMachineBatchAttemptHistory(1)
	wrongLink[1].GetActivityTaskStartedEventAttributes().ScheduledEventId = 9
	matching := expectedMachineBatchAttemptHistory(1)
	matching = append([]*historypb.HistoryEvent{{EventId: 7, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{ScheduledEventId: 6, Attempt: 2}}}}, matching...)
	cases := []struct {
		name          string
		events        []*historypb.HistoryEvent
		historyErr    error
		missingRun    bool
		latestRun     string
		latestErr     error
		want, wantErr bool
	}{
		{name: "matching first attempt in the bound run", events: matching, want: true},
		{name: "an existing replacement keeps the original reservation", latestRun: "replacement-run"},
		{name: "unavailable latest execution keeps the reservation", latestErr: serviceerror.NewUnavailable("private execution error"), wantErr: true},
		{name: "later retry cannot release an earlier possible commit", events: expectedMachineBatchAttemptHistory(2)},
		{name: "unknown attempt remains reserved", events: expectedMachineBatchAttemptHistory(0)},
		{name: "missing start is not proof of non-dispatch", events: expectedMachineBatchAttemptHistory(1)[:1]},
		{name: "scheduled activity ID must match", events: wrongID},
		{name: "scheduled activity type must match", events: wrongType},
		{name: "started event must refer to the scheduled event", events: wrongLink},
		{name: "unavailable history remains reserved", historyErr: serviceerror.NewNotFound("private history message"), wantErr: true},
		{name: "missing run identity cannot query latest history", events: expectedMachineBatchAttemptHistory(1), missingRun: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			converter := temporal.GetDefaultFailureConverter()
			rejection := converter.FailureToError(&failurepb.Failure{Message: "private activity message", Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("private application message", swe.ErrTypeNICoInvalidArgument, nil)), FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}, ActivityId: "batch-create", ScheduledEventId: 10, StartedEventId: 12}}})
			client := &tmocks.Client{}
			runID := "recorded-run"
			if tc.missingRun {
				runID = ""
			} else {
				latestRun := runID
				if tc.latestRun != "" {
					latestRun = tc.latestRun
				}
				client.On("DescribeWorkflowExecution", ctx, "recorded-workflow", "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, Execution: &commonpb.WorkflowExecution{RunId: latestRun}}}, tc.latestErr).Once()
				if tc.latestRun == "" && tc.latestErr == nil {
					client.On("GetWorkflowHistory", ctx, "recorded-workflow", runID, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(&expectedMachineBatchHistory{events: tc.events, err: tc.historyErr}).Once()
				}
			}
			verified, err := VerifyExpectedMachineBatchRejection(ctx, client, "recorded-workflow", runID, rejection)
			require.Equal(t, tc.want, verified)
			if tc.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
			client.AssertExpectations(t)
			if tc.missingRun {
				require.Empty(t, client.Calls)
			}
		})
	}
}

func TestVerifyExpectedMachineBatchNotDispatched(t *testing.T) {
	makeHistory := func() []*historypb.HistoryEvent {
		return []*historypb.HistoryEvent{
			{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{WorkflowType: &commonpb.WorkflowType{Name: "CreateExpectedMachines"}, OriginalExecutionRunId: "original-run", FirstExecutionRunId: "original-run", Attempt: 1}}},
			{EventId: 2, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_WorkflowTaskScheduledEventAttributes{WorkflowTaskScheduledEventAttributes: &historypb.WorkflowTaskScheduledEventAttributes{}}},
			{EventId: 3, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT, Attributes: &historypb.HistoryEvent_WorkflowExecutionTimedOutEventAttributes{WorkflowExecutionTimedOutEventAttributes: &historypb.WorkflowExecutionTimedOutEventAttributes{}}},
		}
	}
	scheduled := makeHistory()
	scheduled[1] = &historypb.HistoryEvent{EventId: 2, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}}}}
	successor := makeHistory()
	successor[2].GetWorkflowExecutionTimedOutEventAttributes().NewExecutionRunId = "successor-run"
	reset := makeHistory()
	reset[0].GetWorkflowExecutionStartedEventAttributes().OriginalExecutionRunId = "before-reset"
	marker := makeHistory()
	marker[1] = &historypb.HistoryEvent{EventId: 2, EventType: enumspb.EVENT_TYPE_MARKER_RECORDED}
	cases := []struct {
		name          string
		events        []*historypb.HistoryEvent
		historyErr    error
		latestRun     string
		want, wantErr bool
	}{
		{name: "complete original closed history with no scheduled activity", events: makeHistory(), want: true},
		{name: "scheduled activity without a start remains unknown", events: scheduled},
		{name: "incomplete history cannot establish closure", events: makeHistory()[:2]},
		{name: "unavailable history retains reservation", historyErr: serviceerror.NewUnavailable("private history error"), wantErr: true},
		{name: "a terminal successor retains reservation", events: successor},
		{name: "latest replacement cannot prove original non-dispatch", latestRun: "replacement-run"},
		{name: "reset lineage retains reservation", events: reset},
		{name: "unexpected command evidence is not ignored", events: marker},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			stc := &tmocks.Client{}
			latest := "original-run"
			if tc.latestRun != "" {
				latest = tc.latestRun
			}
			stc.On("DescribeWorkflowExecution", ctx, "original-workflow", "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_TIMED_OUT, Execution: &commonpb.WorkflowExecution{RunId: latest}}}, nil).Once()
			if tc.latestRun == "" {
				stc.On("GetWorkflowHistory", ctx, "original-workflow", "original-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(&expectedMachineBatchHistory{events: tc.events, err: tc.historyErr}).Once()
			}
			verified, err := VerifyExpectedMachineBatchNotDispatched(ctx, stc, "original-workflow", "original-run")
			require.Equal(t, tc.want, verified)
			if tc.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
			stc.AssertExpectations(t)
		})
	}
}
