// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"sync"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExecuteRuleBasedOperation(t *testing.T) {
	t.Run("parallel additional steps retain their own targets options and counts", func(t *testing.T) {
		env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
		env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, workflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})
		registerTaskUpdateActivities(env)
		power := func(context.Context, common.Target, operations.PowerControlTaskInfo) error { return nil }
		env.RegisterActivityWithOptions(power, activity.RegisterOptions{Name: activitypkg.NamePowerControl})
		env.OnActivity(activitypkg.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(nil)
		var mu sync.Mutex
		seen := make(map[devicetypes.ComponentType][]string)
		env.OnActivity(activitypkg.NamePowerControl, mock.Anything, mock.Anything, mock.Anything).Return(
			func(_ context.Context, target common.Target, options operations.PowerControlTaskInfo) error {
				mu.Lock()
				defer mu.Unlock()
				seen[target.Type] = target.Identifiers
				require.Equal(t, target.Type == devicetypes.ComponentTypeCompute, options.OverrideReadinessCheck)
				return nil
			})
		main := operationrules.SequenceStep{Stage: 1, ComponentType: devicetypes.ComponentTypeNVSwitch,
			MainOperation: operationrules.ActionConfig{Name: operationrules.ActionSleep,
				Parameters: map[string]any{operationrules.ParamDuration: "1ms"}}}
		info := task.ExecutionInfo{TaskID: uuid.New(), OperationType: taskcommon.TaskTypeFirmwareControl,
			OperationInfo:  []byte(`{"operation":1}`),
			Components:     []task.WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "original-switch"}},
			RuleDefinition: &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{main}}}
		for _, typ := range []devicetypes.ComponentType{devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch} {
			bound := task.ExecutionStep{Definition: operationrules.SequenceStep{Stage: 9, ComponentType: typ,
				MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl,
					Parameters: map[string]any{operationrules.ParamOperation: "power_on"}}}}
			if typ == devicetypes.ComponentTypeCompute {
				bound.Components = firmwareTestComponents("compute-1", "compute-2")
				bound.OperationInfo = []byte(`{"operation":1,"override_readiness_check":true}`)
			} else {
				bound.Components = []task.WorkflowComponent{{Type: typ, ComponentID: "additional-switch"}}
			}
			info.AdditionalSteps = append(info.AdditionalSteps, bound)
		}
		env.ExecuteWorkflow(func(ctx workflow.Context, input task.ExecutionInfo) (*report.Report, error) {
			continuations, err := additionalRuleContinuations(ctx, &input)
			if err != nil {
				return nil, err
			}
			targets := buildTargets(&input)
			return executeRuleBasedOperation(ctx, input.TaskID, targets, targets, nil, input.RuleDefinition, continuations...)
		}, info)
		require.NoError(t, env.GetWorkflowError())
		var got report.Report
		require.NoError(t, env.GetWorkflowResult(&got))
		require.Len(t, got.Stages, 2)
		require.Equal(t, 9, got.Stages[1].Number)
		require.Equal(t, report.StatusCompleted, got.Stages[1].Status)
		require.Len(t, got.Stages[1].Steps, 2)
		require.Equal(t, 2, got.Stages[1].Steps[0].TotalComponents)
		require.Equal(t, 1, got.Stages[1].Steps[1].TotalComponents)
		mu.Lock()
		defer mu.Unlock()
		require.Equal(t, []string{"compute-1", "compute-2"}, seen[devicetypes.ComponentTypeCompute])
		require.Equal(t, []string{"additional-switch"}, seen[devicetypes.ComponentTypeNVSwitch])
	})
}
