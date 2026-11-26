// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/message"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestVerifyReachability(t *testing.T) {
	for _, tc := range []struct {
		name       string
		states     map[string]operations.PowerStatus
		requireAll bool
		wantError  bool
		legacy     bool
	}{
		{"missing target", map[string]operations.PowerStatus{"a": operations.PowerStatusOn}, true, true, false},
		{"unrelated response cannot replace missing target", map[string]operations.PowerStatus{"a": operations.PowerStatusOn, "other": operations.PowerStatusOn}, true, true, false},
		{"empty response is not reachable", nil, false, true, false},
		{"one response suffices for any", map[string]operations.PowerStatus{"a": operations.PowerStatusOff}, false, false, false},
		{"successful unknown response counts", map[string]operations.PowerStatus{"a": operations.PowerStatusUnknown, "b": operations.PowerStatusOff}, true, false, false},
		{"old history retains empty response decision", nil, false, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			if tc.legacy {
				env.OnGetVersion("reachability-response-presence", workflow.DefaultVersion, workflow.Version(1)).Return(workflow.DefaultVersion)
			}
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(tc.states, nil)
			env.ExecuteWorkflow(func(ctx workflow.Context) error {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: time.Second})
				target := common.Target{Type: devicetypes.ComponentTypeCompute, IdentifierType: common.IdentifierTypeMACAddress, Identifiers: []string{"a", "b"}}
				return verifyReachability(ctx, map[devicetypes.ComponentType]common.Target{target.Type: target}, []string{"Compute"}, time.Second, time.Second, tc.requireAll, 0)
			})
			if tc.wantError {
				require.ErrorContains(t, env.GetWorkflowError(), "timeout")
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
		})
	}
}

func TestVerifyReachabilityEnforcesDeadlineBetweenBatches(t *testing.T) {
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
	env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).
		After(2*time.Second).
		Return(map[string]operations.PowerStatus{
			"a": operations.PowerStatusOn,
			"b": operations.PowerStatusOn,
		}, nil)

	env.ExecuteWorkflow(func(ctx workflow.Context) error {
		ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: 5 * time.Second})
		target := common.Target{
			Type:           devicetypes.ComponentTypeCompute,
			IdentifierType: common.IdentifierTypeMACAddress,
			Identifiers:    []string{"a", "b"},
		}
		return verifyReachability(
			ctx,
			map[devicetypes.ComponentType]common.Target{target.Type: target},
			[]string{"Compute"},
			3*time.Second,
			time.Second,
			true,
			1,
		)
	})

	require.ErrorContains(t, env.GetWorkflowError(), "timeout")
}

func TestVerifyPowerStatus(t *testing.T) {
	for _, tc := range []struct {
		name      string
		states    map[string]operations.PowerStatus
		wantError bool
		legacy    bool
	}{
		{"partial response", map[string]operations.PowerStatus{"a": operations.PowerStatusOn}, true, false},
		{"empty response", nil, true, false},
		{"all targets match", map[string]operations.PowerStatus{"a": operations.PowerStatusOn, "b": operations.PowerStatusOn}, false, false},
		{"old history retains partial response decision", map[string]operations.PowerStatus{"a": operations.PowerStatusOn}, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			if tc.legacy {
				env.OnGetVersion("power-status-response-presence", workflow.DefaultVersion, workflow.Version(1)).Return(workflow.DefaultVersion)
			}
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(tc.states, nil)
			env.ExecuteWorkflow(func(ctx workflow.Context) error {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: time.Second})
				return verifyPowerStatus(ctx, common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"a", "b"}}, "on", time.Second, time.Second)
			})
			if tc.wantError {
				require.ErrorContains(t, env.GetWorkflowError(), "timeout")
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
		})
	}
	for _, tc := range []struct {
		name        string
		recoverRead bool
		legacy      bool
	}{
		{name: "query error retained at timeout"},
		{name: "legacy timeout retains query error", legacy: true},
		{name: "successful read clears stale query error", recoverRead: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			if tc.legacy {
				env.OnGetVersion("power-status-poll-deadline", workflow.DefaultVersion, workflow.Version(1)).Return(workflow.DefaultVersion)
			}
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			first := env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(nil, errors.New("BMC authentication\ndenied"))
			if tc.recoverRead {
				first.Once()
				env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(map[string]operations.PowerStatus{"a": operations.PowerStatusOff}, nil)
			}
			env.ExecuteWorkflow(func(ctx workflow.Context) error {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: time.Second, RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
				return verifyPowerStatus(ctx, common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"a"}}, "on", 2*time.Second, time.Second)
			})
			err := env.GetWorkflowError()
			require.ErrorContains(t, err, "timeout")
			if tc.recoverRead {
				require.NotContains(t, err.Error(), "BMC authentication")
			} else {
				require.ErrorContains(t, err, "last status query failed")
				require.Contains(t, message.ForFailure(err), "BMC authentication denied")
			}
			env.AssertExpectations(t)
		})
	}

}

func TestExecuteWaitBringUpAction(t *testing.T) {
	for _, tc := range []struct {
		name        string
		recoverRead bool
	}{
		{name: "query error retained at timeout"},
		{name: "successful read clears stale query error", recoverRead: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			env.RegisterActivityWithOptions(mockGetBringUpStatus, activity.RegisterOptions{Name: activitypkg.NameGetBringUpStatus})
			first := env.OnActivity(mockGetBringUpStatus, mock.Anything, mock.Anything).Return(nil, errors.New("Core status service\nunavailable"))
			if tc.recoverRead {
				first.Once()
				env.OnActivity(mockGetBringUpStatus, mock.Anything, mock.Anything).Return(&activitypkg.GetBringUpStatusResult{States: map[string]operations.MachineBringUpState{"a": operations.MachineBringUpStateMachineNotCreated}}, nil)
			}
			env.ExecuteWorkflow(func(ctx workflow.Context) error {
				ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{StartToCloseTimeout: time.Second, RetryPolicy: &temporal.RetryPolicy{MaximumAttempts: 1}})
				return executeWaitBringUpAction(actionExecutionContext{workflowContext: ctx, target: common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"a"}}, config: operationrules.ActionConfig{Timeout: 2 * time.Second, PollInterval: time.Second}})
			})
			err := env.GetWorkflowError()
			require.ErrorContains(t, err, "timed out waiting for compute bring-up")
			if tc.recoverRead {
				require.NotContains(t, err.Error(), "Core status service")
			} else {
				require.Contains(t, message.ForFailure(err), "Core status service unavailable")
			}
			env.AssertExpectations(t)
		})
	}
}
