// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskactivity "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	targetcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	"testing"
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
			var firstErr error
			if tc.firstError {
				firstErr = errors.New("submission response lost")
			}
			env.OnActivity(taskactivity.NameResetNVOS, mock.Anything, first).Return(firstErr).Once()
			env.OnActivity(taskactivity.NameResetNVOS, mock.Anything, second).Return(nil).Once()
			if !tc.firstError {
				env.OnActivity(taskactivity.NameNVOSResetCompleted, mock.Anything, first).Return(false, nil).Once()
				env.OnActivity(taskactivity.NameNVOSResetCompleted, mock.Anything, first).Return(true, nil).Once()
			}
			env.OnActivity(taskactivity.NameNVOSResetCompleted, mock.Anything, second).Return(true, nil).Once()
			env.ExecuteWorkflow(componentReset, task.ExecutionInfo{TaskID: uuid.New(), Components: []task.WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "first"}, {Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "second"}}}, &operations.ComponentResetTaskInfo{})
			require.True(t, env.IsWorkflowCompleted())
			require.Equal(t, tc.firstError, env.GetWorkflowError() != nil)
			env.AssertExpectations(t)
			if tc.firstError {
				env.AssertNotCalled(t, taskactivity.NameNVOSResetCompleted, mock.Anything, first)
			}
			env.AssertCalled(t, taskactivity.NameUpdateTaskStatus, mock.Anything, mock.MatchedBy(func(update *task.TaskStatusUpdate) bool {
				if tc.firstError {
					return update.Status == common.TaskStatusFailed
				}
				return update.Status == common.TaskStatusCompleted
			}))
		})
	}
}
