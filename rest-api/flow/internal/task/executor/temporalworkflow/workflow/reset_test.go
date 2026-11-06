// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"encoding/json"
	"errors"
	"sync/atomic"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
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
		name       string
		firstError bool
	}{{name: "completion"}, {name: "unknown submit does not poll old completion", firstError: true}} {
		t.Run(tc.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameResetNVOS})
			env.RegisterActivityWithOptions(func(context.Context, targetcommon.Target) (bool, error) { return false, nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameNVOSResetCompleted})
			env.RegisterActivityWithOptions(func(context.Context, *task.TaskStatusUpdate) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskStatus})
			env.RegisterActivityWithOptions(func(context.Context, *task.TaskReportUpdate) error { return nil }, sdkactivity.RegisterOptions{Name: taskactivity.NameUpdateTaskReport})
			env.OnActivity(taskactivity.NameUpdateTaskStatus, mock.Anything, mock.Anything).Return(nil)
			env.OnActivity(taskactivity.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(nil)
			first := targetcommon.Target{Type: devicetypes.ComponentTypeNVSwitch, IdentifierType: targetcommon.IdentifierTypeManagerID, Identifiers: []string{"first"}}
			second := first
			second.Identifiers = []string{"second"}
			var secondSubmitted atomic.Bool
			var secondCompleted atomic.Bool
			var firstErr error
			if tc.firstError {
				firstErr = errors.New("submission response lost")
			}
			env.OnActivity(taskactivity.NameResetNVOS, mock.Anything, first).Return(firstErr).Once()
			env.OnActivity(taskactivity.NameResetNVOS, mock.Anything, second).Run(func(mock.Arguments) { secondSubmitted.Store(true) }).Return(nil).Once()
			if !tc.firstError {
				env.OnActivity(taskactivity.NameNVOSResetCompleted, mock.Anything, first).Run(func(mock.Arguments) { require.True(t, secondSubmitted.Load()) }).Return(false, nil).Once()
				env.OnActivity(taskactivity.NameNVOSResetCompleted, mock.Anything, first).Run(func(mock.Arguments) { require.True(t, secondCompleted.Load()) }).Return(true, nil).Once()
			}
			env.OnActivity(taskactivity.NameNVOSResetCompleted, mock.Anything, second).Run(func(mock.Arguments) { secondCompleted.Store(true) }).Return(true, nil).Once()
			env.ExecuteWorkflow(componentReset, task.ExecutionInfo{TaskID: uuid.New(), Components: []task.WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "first"}, {Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "second"}}}, &operations.ComponentResetTaskInfo{})
			require.True(t, env.IsWorkflowCompleted())
			require.Equal(t, tc.firstError, env.GetWorkflowError() != nil)
			env.AssertExpectations(t)
			if tc.firstError {
				env.AssertNotCalled(t, taskactivity.NameNVOSResetCompleted, mock.Anything, first)
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
				if step.TotalComponents != 2 || step.FailedComponents != 0 {
					return false
				}
				if tc.firstError {
					return step.CompletedComponents == 1 && update.Status == common.TaskStatusFailed
				}
				return step.CompletedComponents == 2 && update.Status == common.TaskStatusCompleted
			}))
		})
	}
}
