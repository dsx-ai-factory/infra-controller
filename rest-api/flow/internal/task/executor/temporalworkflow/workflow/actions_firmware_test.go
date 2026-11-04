// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"sync"
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
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func firmwareStatusTestWorkflow(
	ctx temporalworkflow.Context,
	target common.Target,
	pollTimeout time.Duration,
) error {
	ctx = temporalworkflow.WithActivityOptions(ctx, temporalworkflow.ActivityOptions{
		StartToCloseTimeout: time.Second,
		RetryPolicy: &temporal.RetryPolicy{
			MaximumAttempts: 1,
		},
	})
	return executeFirmwareControlAction(actionExecutionContext{
		workflowContext: ctx,
		config: operationrules.ActionConfig{
			Name: operationrules.ActionFirmwareControl,
			Parameters: map[string]any{
				operationrules.ParamPollInterval: "1s",
				operationrules.ParamPollTimeout:  pollTimeout,
			},
		},
		target: target,
		operationInfo: &operations.FirmwareControlTaskInfo{
			Operation: operations.FirmwareOperationUpgrade,
		},
	})
}

func stubStartFirmwareUpdate(
	_ context.Context,
	_ common.Target,
	_ operations.FirmwareControlTaskInfo,
) error {
	return nil
}

func stubGetFirmwareUpdateStatus(
	_ context.Context,
	_ common.Target,
) (*activitypkg.GetFirmwareStatusResult, error) {
	return nil, nil
}

func firmwareStatus(
	componentID string,
	state operations.FirmwareUpdateState,
) operations.FirmwareUpdateStatus {
	return operations.FirmwareUpdateStatus{
		ComponentID: componentID,
		State:       state,
	}
}

func TestExecuteFirmwareControlAction(t *testing.T) {
	completed := operations.FirmwareUpdateStateCompleted
	failed := operations.FirmwareUpdateStateFailed
	queued := operations.FirmwareUpdateStateQueued

	tests := []struct {
		name              string
		responses         []map[string]operations.FirmwareUpdateStatus
		pollTimeout       time.Duration
		legacyHistory     bool
		wantErrorContains []string
		wantMinimumPolls  int
	}{
		{
			name: "all requested components complete",
			responses: []map[string]operations.FirmwareUpdateStatus{{
				"comp-1": firmwareStatus("comp-1", completed),
				"comp-2": firmwareStatus("comp-2", completed),
			}},
			pollTimeout:      5 * time.Second,
			wantMinimumPolls: 1,
		},
		{
			name: "empty response remains pending",
			responses: []map[string]operations.FirmwareUpdateStatus{
				{},
				{
					"comp-1": firmwareStatus("comp-1", completed),
					"comp-2": firmwareStatus("comp-2", completed),
				},
			},
			pollTimeout:      5 * time.Second,
			wantMinimumPolls: 2,
		},
		{
			name: "missing terminal status remains unresolved until reported again",
			responses: []map[string]operations.FirmwareUpdateStatus{
				{"comp-1": firmwareStatus("comp-1", completed)},
				{"comp-2": firmwareStatus("comp-2", completed)},
				{
					"comp-1": firmwareStatus("comp-1", completed),
					"comp-2": firmwareStatus("comp-2", completed),
				},
			},
			pollTimeout:      5 * time.Second,
			wantMinimumPolls: 3,
		},
		{
			name: "failure waits for remaining component",
			responses: []map[string]operations.FirmwareUpdateStatus{
				{
					"comp-1": firmwareStatus("comp-1", failed),
					"comp-2": firmwareStatus("comp-2", queued),
				},
				{
					"comp-1": firmwareStatus("comp-1", failed),
					"comp-2": firmwareStatus("comp-2", completed),
				},
			},
			pollTimeout:       5 * time.Second,
			wantErrorContains: []string{"firmware update failed", "comp-1"},
			wantMinimumPolls:  2,
		},
		{
			name: "unexpected component does not satisfy requested target",
			responses: []map[string]operations.FirmwareUpdateStatus{
				{"other": firmwareStatus("other", completed)},
				{
					"comp-1": firmwareStatus("comp-1", completed),
					"comp-2": firmwareStatus("comp-2", completed),
				},
			},
			pollTimeout:      5 * time.Second,
			wantMinimumPolls: 2,
		},
		{
			name: "timeout reports failed and unresolved components",
			responses: []map[string]operations.FirmwareUpdateStatus{{
				"comp-1": firmwareStatus("comp-1", failed),
				"comp-2": firmwareStatus("comp-2", queued),
			}},
			pollTimeout:       2 * time.Second,
			wantErrorContains: []string{"timed out", "failed components: [comp-1]", "unresolved components: [comp-2]"},
			wantMinimumPolls:  2,
		},
		{
			name:             "legacy history retains empty response completion",
			responses:        []map[string]operations.FirmwareUpdateStatus{{}},
			pollTimeout:      5 * time.Second,
			legacyHistory:    true,
			wantMinimumPolls: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			if tc.legacyHistory {
				env.OnGetVersion(
					firmwareStatusTargetReconciliationChangeID,
					temporalworkflow.DefaultVersion,
					temporalworkflow.Version(1),
				).Return(temporalworkflow.DefaultVersion).Once()
			}

			env.RegisterActivityWithOptions(
				stubStartFirmwareUpdate,
				activity.RegisterOptions{Name: activitypkg.NameFirmwareControl},
			)
			env.RegisterActivityWithOptions(
				stubGetFirmwareUpdateStatus,
				activity.RegisterOptions{Name: activitypkg.NameGetFirmwareStatus},
			)
			env.OnActivity(
				stubStartFirmwareUpdate,
				mock.Anything,
				mock.Anything,
				mock.Anything,
			).Return(nil).Once()

			var statusMu sync.Mutex
			statusCalls := 0
			env.OnActivity(
				stubGetFirmwareUpdateStatus,
				mock.Anything,
				mock.Anything,
			).Return(func(_ context.Context, _ common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
				statusMu.Lock()
				defer statusMu.Unlock()
				responseIndex := min(statusCalls, len(tc.responses)-1)
				statusCalls++
				return &activitypkg.GetFirmwareStatusResult{Statuses: tc.responses[responseIndex]}, nil
			})

			target := common.Target{
				Type:        devicetypes.ComponentTypeCompute,
				Identifiers: []string{"comp-1", "comp-2"},
			}
			env.ExecuteWorkflow(firmwareStatusTestWorkflow, target, tc.pollTimeout)

			require.True(t, env.IsWorkflowCompleted())
			workflowErr := env.GetWorkflowError()
			if len(tc.wantErrorContains) == 0 {
				require.NoError(t, workflowErr)
			} else {
				for _, expected := range tc.wantErrorContains {
					require.ErrorContains(t, workflowErr, expected)
				}
			}
			statusMu.Lock()
			defer statusMu.Unlock()
			require.GreaterOrEqual(t, statusCalls, tc.wantMinimumPolls)
			env.AssertExpectations(t)
		})
	}
}
