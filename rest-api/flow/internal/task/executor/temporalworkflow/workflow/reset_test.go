// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskactivity "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	targetcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestComponentReset(t *testing.T) {
	for _, tc := range []struct {
		name                string
		firstError          bool
		pollError           bool
		resetTarget         string
		definitive          bool
		acceptedBeforeBlock bool
		preflightError      bool
		readinessChanged    bool
	}{{name: "later block preserves accepted observation", acceptedBeforeBlock: true}, {name: "earlier submission error retains readiness summary", acceptedBeforeBlock: true, firstError: true}, {name: "completion"}, {name: "blocked rack stops before mutation", preflightError: true}, {name: "readiness changes before submission", readinessChanged: true, firstError: true}, {name: "SDN completion", resetTarget: "sdn"}, {name: "SDN rejection is terminal", resetTarget: "sdn", firstError: true, definitive: true}, {name: "transient observation recovers without resubmission", pollError: true}, {name: "unknown submit does not poll old completion", firstError: true}} {
		t.Run(tc.name, func(t *testing.T) {
			if tc.acceptedBeforeBlock {
				checkResetAcceptedBeforeReadinessBlock(t, tc.firstError)
				return
			}
			submitActivity, observeActivity := taskactivity.NameSubmitFactoryReset, taskactivity.NameNVOSResetCompleted
			if tc.resetTarget == "" {
				tc.resetTarget = "nvos"
			}
			if tc.resetTarget == "sdn" {
				observeActivity = taskactivity.NameSDNResetCompleted
			}
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target, string) error { return nil }, sdkactivity.RegisterOptions{Name: submitActivity})
			env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NamePrepareFactoryReset})
			env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target) (bool, error) { return false, nil }, sdkactivity.RegisterOptions{Name: observeActivity})
			env.RegisterActivityWithOptions(func(context.Context, *task.TaskStatusUpdate) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskStatus})
			env.RegisterActivityWithOptions(func(context.Context, *task.TaskReportUpdate) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskReport})
			var preflightErr error
			if tc.preflightError {
				preflightErr = errors.New("rack hosts still in use")
			}
			env.OnActivity(taskactivity.NamePrepareFactoryReset, mock.Anything, mock.MatchedBy(func(target targetcommon.Target) bool {
				return len(target.Identifiers) == 2 && target.TraceID == "1234567890abcdef1234567890abcdef"
			})).Return(preflightErr).Once()
			env.OnActivity(taskactivity.NameUpdateTaskStatus, mock.Anything, mock.Anything).Return(nil)
			if !tc.preflightError && !tc.readinessChanged {
				env.OnActivity(taskactivity.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(nil)
			}
			first := targetcommon.Target{TraceID: "1234567890abcdef1234567890abcdef", Type: devicetypes.ComponentTypeNVSwitch, IdentifierType: targetcommon.IdentifierTypeManagerID, Identifiers: []string{"first"}}
			second := first
			second.Identifiers = []string{"second"}
			var secondSubmitted atomic.Bool
			var secondCompleted atomic.Bool
			var firstErr error
			if tc.firstError {
				firstErr = errors.New("submission response lost")
				if tc.definitive {
					firstErr = temporal.NewNonRetryableApplicationError("reset rejected", "SDNResetRejected", nil)
				}
			}
			if tc.readinessChanged {
				firstErr = temporal.NewNonRetryableApplicationError("rack hosts in use", "FactoryResetNotReady", nil)
			}
			submitArgs := func(target targetcommon.Target) []interface{} {
				args := []interface{}{mock.Anything, target}
				args = append(args, tc.resetTarget)
				return args
			}
			if !tc.preflightError {
				env.OnActivity(submitActivity, submitArgs(first)...).Return(firstErr).Once()
				if !tc.readinessChanged {
					env.OnActivity(submitActivity, submitArgs(second)...).Run(func(mock.Arguments) { secondSubmitted.Store(true) }).Return(nil).Once()
					if !tc.firstError {
						var pollErr error
						if tc.pollError {
							pollErr = errors.New("temporary status transport failure")
						}
						env.OnActivity(observeActivity, mock.Anything, first).Run(func(mock.Arguments) { require.True(t, secondSubmitted.Load()) }).Return(false, pollErr).Once()
						env.OnActivity(observeActivity, mock.Anything, first).Run(func(mock.Arguments) { require.True(t, secondCompleted.Load()) }).Return(true, nil).Once()
					}
					env.OnActivity(observeActivity, mock.Anything, second).Run(func(mock.Arguments) { secondCompleted.Store(true) }).Return(true, nil).Once()
				}
			}
			env.ExecuteWorkflow(componentReset, task.ExecutionInfo{TraceID: "1234567890abcdef1234567890abcdef", TaskID: uuid.New(), Components: []task.WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "first"}, {Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "second"}}}, &operations.ComponentResetTaskInfo{Target: tc.resetTarget})
			require.True(t, env.IsWorkflowCompleted())
			require.Equal(t, tc.firstError || tc.preflightError, env.GetWorkflowError() != nil)
			env.AssertExpectations(t)
			if tc.preflightError {
				env.AssertNumberOfCalls(t, submitActivity, 0)
				env.AssertCalled(t, taskactivity.NameUpdateTaskStatus, mock.Anything, mock.MatchedBy(func(update *task.TaskStatusUpdate) bool { return update.Status == common.TaskStatusFailed }))
				return
			}
			if tc.readinessChanged {
				env.AssertNumberOfCalls(t, submitActivity, 1)
			}
			if tc.firstError {
				env.AssertNotCalled(t, observeActivity, mock.Anything, first)
			}
			env.AssertCalled(t, taskactivity.NameUpdateTaskStatus, mock.Anything, mock.MatchedBy(func(update *task.TaskStatusUpdate) bool {
				if update.Status != common.TaskStatusFailed && update.Status != common.TaskStatusCompleted {
					return false
				}
				var rep report.Report
				if json.Unmarshal(update.Report, &rep) != nil || len(rep.Stages) != 1 {
					return false
				}
				step := rep.Stages[0].Steps[0]
				expectedFailed := 0
				if tc.definitive {
					expectedFailed = 1
				}
				if step.TotalComponents != 2 || step.FailedComponents != expectedFailed {
					return false
				}
				if tc.firstError {
					return step.CompletedComponents == map[bool]int{true: 0, false: 1}[tc.readinessChanged] && update.Status == common.TaskStatusFailed
				}
				return step.CompletedComponents == 2 && update.Status == common.TaskStatusCompleted
			}))
		})
	}
}

func checkResetAcceptedBeforeReadinessBlock(t *testing.T, earlierFailure bool) {
	t.Helper()
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NamePrepareFactoryReset})
	var submitted, observed atomic.Int32
	env.RegisterActivityWithOptions(func(_ context.Context, target targetcommon.Target, _ string) error {
		submitted.Add(1)
		if earlierFailure && target.Identifiers[0] == "first" {
			return errors.New("earlier submission response lost")
		}
		if target.Identifiers[0] == "second" {
			return temporal.NewNonRetryableApplicationError("rack hosts in use", "FactoryResetNotReady", nil)
		}
		return nil
	}, sdkactivity.RegisterOptions{Name: taskactivity.NameSubmitFactoryReset})
	env.RegisterActivityWithOptions(func(_ context.Context, target targetcommon.Target) (bool, error) {
		require.Equal(t, []string{"first"}, target.Identifiers)
		observed.Add(1)
		return true, nil
	}, sdkactivity.RegisterOptions{Name: taskactivity.NameNVOSResetCompleted})
	env.RegisterActivityWithOptions(func(context.Context, *task.TaskReportUpdate) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskReport})
	env.RegisterActivityWithOptions(func(context.Context, *task.TaskStatusUpdate) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskStatus})
	env.OnActivity(taskactivity.NameUpdateTaskStatus, mock.Anything, mock.Anything).Return(nil)
	info := task.ExecutionInfo{TaskID: uuid.New()}
	for _, id := range []string{"first", "second", "third"} {
		info.Components = append(info.Components, task.WorkflowComponent{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: id})
	}
	env.ExecuteWorkflow(componentReset, info, &operations.ComponentResetTaskInfo{Target: "nvos"})
	require.ErrorContains(t, env.GetWorkflowError(), "readiness")
	require.EqualValues(t, 2, submitted.Load())
	expectedCompleted := 1
	if earlierFailure {
		expectedCompleted = 0
	}
	require.EqualValues(t, expectedCompleted, observed.Load())
	const expectedSummary = "factory reset stopped: rack readiness check failed; 2 switches were not submitted"
	require.ErrorContains(t, env.GetWorkflowError(), expectedSummary)
	env.AssertCalled(t, taskactivity.NameUpdateTaskStatus, mock.Anything, mock.MatchedBy(func(update *task.TaskStatusUpdate) bool {
		if update.Status != common.TaskStatusFailed || !strings.HasPrefix(update.Message, expectedSummary) {
			return false
		}
		var rep report.Report
		if json.Unmarshal(update.Report, &rep) != nil || len(rep.Stages) != 1 {
			return false
		}
		step := rep.Stages[0].Steps[0]
		return step.TotalComponents == 3 && step.CompletedComponents == expectedCompleted && step.FailedComponents == 0 && strings.HasPrefix(rep.Error, expectedSummary)
	}))
}
