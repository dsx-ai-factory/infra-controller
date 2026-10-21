// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestActivityRetryBudget(t *testing.T) {
	for _, tc := range []struct {
		name string
		step operationrules.SequenceStep
		want time.Duration
	}{
		{"defaults include three attempts", operationrules.SequenceStep{}, time.Hour + 3*time.Second},
		{"exponential backoff capped", operationrules.SequenceStep{
			Timeout: time.Minute,
			RetryPolicy: &operationrules.RetryPolicy{
				MaxAttempts: 4, InitialInterval: time.Second, BackoffCoefficient: 2, MaxInterval: 3 * time.Second,
			},
		}, 4*time.Minute + 6*time.Second},
	} {
		t.Run(tc.name, func(t *testing.T) {
			require.Equal(t, tc.want, activityRetryBudget(tc.step))
		})
	}
}

func TestFirmwarePollTimeout(t *testing.T) {
	for _, tc := range []struct {
		value any
		want  time.Duration
	}{
		{nil, 30 * time.Minute},
		{"45m", 45 * time.Minute},
		{"60m", time.Hour},
	} {
		action := operationrules.ActionConfig{Parameters: map[string]any{operationrules.ParamPollTimeout: tc.value}}
		require.Equal(t, tc.want, firmwarePollTimeout(action))
	}
}

func TestFirmwareControl_LongSequentialStages(t *testing.T) {
	var resolver *operationrules.Resolver
	rule, err := resolver.ResolveRule(context.Background(), taskcommon.TaskTypeFirmwareControl, operationrules.SequenceUpgrade, uuid.Nil, nil)
	require.NoError(t, err)
	info := &operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade}
	req := task.ExecutionInfo{TaskID: uuid.New(), RuleDefinition: &rule.RuleDefinition,
		Components: []task.WorkflowComponent{
			{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"},
			{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "switch-1"},
		},
	}
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	env.SetStartWorkflowOptions(client.StartWorkflowOptions{WorkflowExecutionTimeout: ExecutionTimeout(&req, info, time.Hour)})
	env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})
	registerTaskUpdateActivities(env)
	env.RegisterActivityWithOptions(stubStartFirmwareUpdate, activity.RegisterOptions{Name: activitypkg.NameFirmwareControl})
	env.RegisterActivityWithOptions(stubGetFirmwareUpdateStatus, activity.RegisterOptions{Name: activitypkg.NameGetFirmwareStatus})
	expectTaskUpdateActivities(env)
	env.OnActivity(stubStartFirmwareUpdate, mock.Anything, mock.Anything, mock.Anything).Return(nil)
	var computeReady, switchReady atomic.Bool
	env.RegisterDelayedCallback(func() { computeReady.Store(true) }, 58*time.Minute)
	env.RegisterDelayedCallback(func() { switchReady.Store(true) }, 78*time.Minute)
	env.OnActivity(stubGetFirmwareUpdateStatus, mock.Anything, mock.Anything).Return(
		func(_ context.Context, target common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
			ready := computeReady.Load()
			if target.Type == devicetypes.ComponentTypeNVSwitch {
				ready = switchReady.Load()
			}
			state := operations.FirmwareUpdateStateQueued
			if ready {
				state = operations.FirmwareUpdateStateCompleted
			}
			id := target.Identifiers[0]
			return &activitypkg.GetFirmwareStatusResult{Statuses: map[string]operations.FirmwareUpdateStatus{id: firmwareStatus(id, state)}}, nil
		})
	started := env.Now()
	env.ExecuteWorkflow(firmwareControl, req, info)
	require.NoError(t, env.GetWorkflowError())
	require.Equal(t, 78*time.Minute, env.Now().Sub(started))
}

func TestFirmwareStepExecutionTimeout(t *testing.T) {
	step := operationrules.SequenceStep{
		Timeout: time.Minute, MaxParallel: 2, DelayAfter: time.Second,
		RetryPolicy: &operationrules.RetryPolicy{MaxAttempts: 2, InitialInterval: time.Second, BackoffCoefficient: 2},
		MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl,
			Parameters: map[string]any{operationrules.ParamPollTimeout: "60m"}},
		PreOperation: []operationrules.ActionConfig{{Name: operationrules.ActionSleep,
			Parameters: map[string]any{operationrules.ParamDuration: "5s"}}},
		PostOperation: []operationrules.ActionConfig{{Name: operationrules.ActionVerifyPowerStatus,
			Timeout: time.Minute, PollInterval: time.Second}},
	}
	for _, count := range []int{1, 2, 3, 5} {
		batches := (count + 1) / 2
		want := 2*time.Minute + 6*time.Second + time.Duration(batches)*(65*time.Minute+3*time.Second)
		require.Equal(t, want, firmwareStepExecutionTimeout(step, count))
	}
}

func TestExecutionTimeout(t *testing.T) {
	compute := devicetypes.ComponentTypeCompute
	switches := devicetypes.ComponentTypeNVSwitch
	step := operationrules.SequenceStep{
		ComponentType: compute, Stage: 2, Timeout: time.Minute,
		RetryPolicy: &operationrules.RetryPolicy{MaxAttempts: 1, InitialInterval: time.Second, BackoffCoefficient: 2},
		MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl,
			Parameters: map[string]any{operationrules.ParamPollTimeout: "60m"}},
	}
	for _, tc := range []struct {
		name        string
		secondStage int
		withSwitch  bool
		want        time.Duration
	}{
		{"parallel uses maximum", 2, true, 123 * time.Minute},
		{"sparse sequential stages sum", 8, true, 186 * time.Minute},
		{"absent targets skipped", 8, false, 123 * time.Minute},
	} {
		t.Run(tc.name, func(t *testing.T) {
			second := step
			second.ComponentType, second.Stage = switches, tc.secondStage
			info := task.ExecutionInfo{
				RuleDefinition: &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{step, second}},
				Components:     []task.WorkflowComponent{{Type: compute, ComponentID: "compute-1"}},
			}
			if tc.withSwitch {
				info.Components = append(info.Components, task.WorkflowComponent{Type: switches, ComponentID: "switch-1"})
			}
			require.Equal(t, tc.want, ExecutionTimeout(&info, nil, time.Hour))
		})
	}
}

func firmwareDeadlineTestWorkflow(ctx temporalworkflow.Context, pollTimeout, interval time.Duration) (time.Duration, error) {
	ctx = temporalworkflow.WithActivityOptions(ctx, temporalworkflow.ActivityOptions{
		StartToCloseTimeout: 45 * time.Minute,
		RetryPolicy:         &temporal.RetryPolicy{MaximumAttempts: 2, InitialInterval: time.Second},
	})
	started := temporalworkflow.Now(ctx)
	err := executeFirmwareControlAction(actionExecutionContext{
		workflowContext: ctx,
		config: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl,
			Parameters: map[string]any{operationrules.ParamPollTimeout: pollTimeout, operationrules.ParamPollInterval: interval}},
		target: common.Target{Type: devicetypes.ComponentTypeCompute, Identifiers: []string{"comp-1"}},
	})
	return temporalworkflow.Now(ctx).Sub(started), err
}

func TestFirmwarePollingDeadline(t *testing.T) {
	for _, tc := range []struct {
		name        string
		delay       time.Duration
		readyAt     time.Duration
		complete    bool
		legacy      bool
		wantSuccess bool
		cancel      bool
		retry       bool
	}{
		{name: "completed at 58 minutes", readyAt: 58 * time.Minute, complete: true, wantSuccess: true},
		{name: "completed just before deadline", delay: time.Hour - time.Second, complete: true, wantSuccess: true},
		{name: "query exceeds remaining budget", delay: 61 * time.Minute, complete: true},
		{name: "response exactly at deadline is unresolved", delay: time.Hour, complete: true},
		{name: "sleep capped to remaining budget", delay: 59 * time.Minute},
		{name: "legacy keeps late response", delay: 61 * time.Minute, complete: true, legacy: true, wantSuccess: true},
		{name: "cancellation interrupts query", delay: time.Hour, cancel: true},
		{name: "retry cannot extend deadline", delay: 31 * time.Minute, retry: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			if tc.legacy {
				env.OnGetVersion(firmwareTimeoutBudgetChangeID, temporalworkflow.DefaultVersion, temporalworkflow.Version(1)).Return(temporalworkflow.DefaultVersion)
			}
			env.RegisterActivityWithOptions(stubStartFirmwareUpdate, activity.RegisterOptions{Name: activitypkg.NameFirmwareControl})
			env.RegisterActivityWithOptions(stubGetFirmwareUpdateStatus, activity.RegisterOptions{Name: activitypkg.NameGetFirmwareStatus})
			env.OnActivity(stubStartFirmwareUpdate, mock.Anything, mock.Anything, mock.Anything).Return(nil)
			var ready atomic.Bool
			ready.Store(tc.complete && tc.readyAt == 0)
			if tc.readyAt > 0 {
				env.RegisterDelayedCallback(func() { ready.Store(true) }, tc.readyAt)
			}
			if tc.cancel {
				env.RegisterDelayedCallback(env.CancelWorkflow, time.Minute)
			}
			var calls atomic.Int32
			env.OnActivity(stubGetFirmwareUpdateStatus, mock.Anything, mock.Anything).
				After(tc.delay).Return(func(_ context.Context, _ common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
				calls.Add(1)
				if tc.retry {
					return nil, errors.New("status backend unavailable")
				}
				state := operations.FirmwareUpdateStateQueued
				if ready.Load() {
					state = operations.FirmwareUpdateStateCompleted
				}
				return &activitypkg.GetFirmwareStatusResult{Statuses: map[string]operations.FirmwareUpdateStatus{
					"comp-1": firmwareStatus("comp-1", state),
				}}, nil
			})
			started := env.Now()
			env.ExecuteWorkflow(firmwareDeadlineTestWorkflow, time.Hour, 2*time.Minute)
			if tc.cancel {
				require.True(t, temporal.IsCanceledError(env.GetWorkflowError()))
				require.Equal(t, time.Minute, env.Now().Sub(started))
			} else if tc.wantSuccess {
				require.NoError(t, env.GetWorkflowError())
			} else {
				require.ErrorContains(t, env.GetWorkflowError(), "unresolved components: [comp-1]")
				require.Equal(t, time.Hour, env.Now().Sub(started))
			}
		})
	}
}
