// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"
	"testing"
	"time"
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
				return verifyReachability(ctx, map[devicetypes.ComponentType]common.Target{target.Type: target}, []string{"Compute"}, time.Second, time.Second, tc.requireAll)
			})
			if tc.wantError {
				require.ErrorContains(t, env.GetWorkflowError(), "timeout")
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
		})
	}
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
}
