// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package vpc observes the original Site VPC creation without dispatching it again.
package vpc

import (
	"context"
	"errors"
	"time"

	enumspb "go.temporal.io/api/enums/v1"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// ObserveCreation returns either the original VPC, a verified rejection, or
// neither. Running or unproved results return neither with no error; lookup,
// decoding and history failures return an error. It observes for at most one
// second, never waits for a running workflow, and never starts one.
func ObserveCreation(ctx context.Context, stc client.Client, workflowID string) (created *corev1.Vpc, rejected bool, err error) {
	ctx, cancel := context.WithTimeout(ctx, time.Second)
	defer cancel()
	execution, err := stc.DescribeWorkflowExecution(ctx, workflowID, "")
	if err != nil {
		return nil, false, err
	}
	info := execution.GetWorkflowExecutionInfo()
	if info.GetType().GetName() != "CreateVPCV2" || info.GetExecution().GetWorkflowId() != workflowID || info.GetExecution().GetRunId() == "" {
		return nil, false, nil
	}
	if info.GetStatus() != enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED && info.GetStatus() != enumspb.WORKFLOW_EXECUTION_STATUS_FAILED {
		return nil, false, nil
	}
	runID := info.GetExecution().GetRunId()
	var result corev1.Vpc
	err = stc.GetWorkflow(ctx, workflowID, runID).GetWithOptions(ctx, &result, client.WorkflowRunGetOptions{DisableFollowingRuns: true})
	if err == nil {
		return &result, false, nil
	}
	if info.GetStatus() != enumspb.WORKFLOW_EXECUTION_STATUS_FAILED {
		return nil, false, err
	}
	var activityErr *temporal.ActivityError
	var applicationErr *temporal.ApplicationError
	if !errors.As(err, &activityErr) || activityErr.ActivityType().GetName() != "CreateVpcOnSite" || !errors.As(err, &applicationErr) {
		return nil, false, err
	}
	switch applicationErr.Type() {
	// These original create responses precede Core's entity commit. AlreadyExists
	// may instead reflect an earlier commit and must remain unconfirmed.
	case swe.ErrTypeInvalidRequest,
		swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound,
		swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented,
		swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied,
		swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition,
		swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
	default:
		return nil, false, err
	}
	if activityErr.ScheduledEventID() <= 0 || activityErr.StartedEventID() <= activityErr.ScheduledEventID() || activityErr.ActivityID() == "" {
		return nil, false, nil
	}
	// Retained histories can come from workers that retried after an uncertain
	// commit. Only the original run's first activity attempt can prove rejection.
	history := stc.GetWorkflowHistory(ctx, workflowID, runID, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT)
	if history == nil {
		return nil, false, nil
	}
	scheduled, started, originalRun := false, false, false
	for history.HasNext() {
		if ctx.Err() != nil {
			return nil, false, ctx.Err()
		}
		event, historyErr := history.Next()
		if historyErr != nil {
			return nil, false, historyErr
		}
		if event == nil {
			return nil, false, nil
		}
		if ctx.Err() != nil {
			return nil, false, ctx.Err()
		}
		if event.GetEventId() == 1 {
			start := event.GetWorkflowExecutionStartedEventAttributes()
			if event.GetEventType() != enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED ||
				start.GetWorkflowType().GetName() != "CreateVPCV2" || start.GetAttempt() != 1 ||
				start.GetOriginalExecutionRunId() != runID || start.GetFirstExecutionRunId() != runID ||
				start.GetContinuedExecutionRunId() != "" || start.GetCronSchedule() != "" {
				return nil, false, nil
			}
			originalRun = true
		}
		if event.GetEventId() == activityErr.ScheduledEventID() {
			attributes := event.GetActivityTaskScheduledEventAttributes()
			scheduled = event.GetEventType() == enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED && attributes.GetActivityType().GetName() == "CreateVpcOnSite" && attributes.GetActivityId() == activityErr.ActivityID()
		}
		if event.GetEventId() == activityErr.StartedEventID() {
			attributes := event.GetActivityTaskStartedEventAttributes()
			started = event.GetEventType() == enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED && attributes.GetScheduledEventId() == activityErr.ScheduledEventID() && attributes.GetAttempt() == 1
		}
		if originalRun && scheduled && started {
			return nil, true, nil
		}
	}
	return nil, false, ctx.Err()
}
