// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func powerDeadlineTestWorkflow(ctx temporalworkflow.Context) error {
	ctx = temporalworkflow.WithActivityOptions(ctx, temporalworkflow.ActivityOptions{
		StartToCloseTimeout: time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 1},
	})
	return verifyPowerStatus(ctx, common.Target{
		Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"a"},
	}, "on", 5*time.Second, 2*time.Second)
}

func TestVerifyPowerStatus_Deadline(t *testing.T) {
	for _, tc := range []struct {
		name        string
		delay       time.Duration
		state       operations.PowerStatus
		legacy      bool
		cancel      bool
		wantError   string
		wantElapsed time.Duration
	}{
		{name: "response before deadline", delay: 4 * time.Second, state: operations.PowerStatusOn, wantElapsed: 4 * time.Second},
		{name: "query cannot extend deadline", delay: 6 * time.Second, state: operations.PowerStatusOn, wantError: "timeout", wantElapsed: 5 * time.Second},
		{name: "sleep uses remaining time", delay: 4 * time.Second, state: operations.PowerStatusOff, wantError: "timeout", wantElapsed: 5 * time.Second},
		{name: "legacy response can exceed deadline", delay: 6 * time.Second, state: operations.PowerStatusOn, legacy: true, wantElapsed: 6 * time.Second},
		{name: "cancel waiting", delay: 6 * time.Second, cancel: true, wantElapsed: time.Second},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			if tc.legacy {
				env.OnGetVersion("power-status-poll-deadline", temporalworkflow.DefaultVersion, temporalworkflow.Version(1)).Return(temporalworkflow.DefaultVersion)
			}
			if tc.cancel {
				env.RegisterDelayedCallback(env.CancelWorkflow, time.Second)
			}
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).After(tc.delay).
				Return(map[string]operations.PowerStatus{"a": tc.state}, nil).Once()
			started := env.Now()
			env.ExecuteWorkflow(powerDeadlineTestWorkflow)
			if tc.cancel {
				require.True(t, temporal.IsCanceledError(env.GetWorkflowError()))
			} else if tc.wantError != "" {
				require.ErrorContains(t, env.GetWorkflowError(), tc.wantError)
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
			require.Equal(t, tc.wantElapsed, env.Now().Sub(started))
		})
	}
}

func TestExecuteActivityBeforeDeadline(t *testing.T) {
	t.Run("delayed workflow task", func(t *testing.T) {
		for _, tc := range []struct {
			name       string
			timerFirst bool
		}{
			{name: "activity completed before timer"},
			{name: "timer fired before activity", timerFirst: true},
		} {
			t.Run(tc.name, func(t *testing.T) {
				activityEvent := `{"eventId":"FIRST","eventTime":"2026-10-12T10:59:59Z","eventType":"EVENT_TYPE_ACTIVITY_TASK_COMPLETED","activityTaskCompletedEventAttributes":{"scheduledEventId":"5","startedEventId":"7"}}`
				timerEvent := `{"eventId":"SECOND","eventTime":"2026-10-12T11:00:00Z","eventType":"EVENT_TYPE_TIMER_FIRED","timerFiredEventAttributes":{"timerId":"6","startedEventId":"6"}}`
				events := activityEvent + "," + timerEvent
				if tc.timerFirst {
					events = strings.ReplaceAll(timerEvent, "SECOND", "FIRST") + "," + strings.ReplaceAll(strings.ReplaceAll(activityEvent, "FIRST", "SECOND"), "10:59:59", "11:00:00")
				}
				events = strings.NewReplacer("FIRST", "8", "SECOND", "9").Replace(events)
				history := `{"events":[
					{"eventId":"1","eventTime":"2026-10-12T10:00:00Z","eventType":"EVENT_TYPE_WORKFLOW_EXECUTION_STARTED","workflowExecutionStartedEventAttributes":{"workflowType":{"name":"DeadlineReplay"},"taskQueue":{"name":"deadline-replay"},"input":%s}},
					{"eventId":"2","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
					{"eventId":"3","eventTime":"2026-10-12T10:00:00Z","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"2"}},
					{"eventId":"4","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"2","startedEventId":"3"}},
					{"eventId":"5","eventType":"EVENT_TYPE_ACTIVITY_TASK_SCHEDULED","activityTaskScheduledEventAttributes":{"activityId":"5","activityType":{"name":"GetPowerStatus"},"taskQueue":{"name":"deadline-replay"}}},
					{"eventId":"6","eventType":"EVENT_TYPE_TIMER_STARTED","timerStartedEventAttributes":{"timerId":"6","startToFireTimeout":"3600s","workflowTaskCompletedEventId":"4"}},
					{"eventId":"7","eventType":"EVENT_TYPE_ACTIVITY_TASK_STARTED","activityTaskStartedEventAttributes":{"scheduledEventId":"5"}},
					` + events + `,
					{"eventId":"10","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
					{"eventId":"11","eventTime":"2026-10-12T11:00:01Z","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"10"}},
					{"eventId":"12","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"10","startedEventId":"11"}},
					{"eventId":"13","eventType":"EVENT_TYPE_WORKFLOW_EXECUTION_COMPLETED","workflowExecutionCompletedEventAttributes":{"workflowTaskCompletedEventId":"12"}}
				]}`
				var checked bool
				assertWorkflowHistoryReplay(t, "DeadlineReplay", func(ctx temporalworkflow.Context) error {
					ctx = temporalworkflow.WithActivityOptions(ctx, temporalworkflow.ActivityOptions{StartToCloseTimeout: time.Hour})
					deadline := temporalworkflow.Now(ctx).Add(time.Hour)
					err := executeActivityBeforeDeadline(ctx, deadline, nil, activitypkg.NameGetPowerStatus, common.Target{})
					checked = true
					require.True(t, temporalworkflow.Now(ctx).After(deadline))
					if tc.timerFirst {
						require.ErrorIs(t, err, context.DeadlineExceeded)
					} else {
						require.NoError(t, err)
					}
					return nil
				}, history)
				require.True(t, checked, "history must resume the waiting workflow")
			})
		}
	})
	for _, tc := range []struct {
		name        string
		budget      time.Duration
		calls       int
		wantElapsed time.Duration
	}{
		{"expired deadline dispatches nothing", 0, 0, 0},
		{"multiple calls share one budget", 5 * time.Second, 2, 5 * time.Second},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			if tc.calls > 0 {
				env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).After(4*time.Second).
					Return(map[string]operations.PowerStatus{"a": operations.PowerStatusOn}, nil)
			}
			started := env.Now()
			env.ExecuteWorkflow(func(ctx temporalworkflow.Context) error {
				ctx = temporalworkflow.WithActivityOptions(ctx, temporalworkflow.ActivityOptions{StartToCloseTimeout: time.Minute})
				deadline := temporalworkflow.Now(ctx).Add(tc.budget)
				for range 2 {
					var result map[string]operations.PowerStatus
					if err := executeActivityBeforeDeadline(ctx, deadline, &result, activitypkg.NameGetPowerStatus, common.Target{}); err != nil {
						return err
					}
				}
				return nil
			})
			require.ErrorContains(t, env.GetWorkflowError(), "deadline exceeded")
			require.Equal(t, tc.wantElapsed, env.Now().Sub(started))
			if tc.calls == 0 {
				env.AssertActivityNotCalled(t, activitypkg.NameGetPowerStatus, mock.Anything, mock.Anything)
			}
		})
	}
}
