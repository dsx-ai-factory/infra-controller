// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	enums "go.temporal.io/api/enums/v1"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExecuteRuleBasedOperation_BestEffort(t *testing.T) {
	for _, tc := range []struct {
		name        string
		failure     error
		wantStatus  string
		group       bool
		dependency  bool
		observation bool
	}{
		{name: "unobserved power through real child", observation: true, wantStatus: "unknown"},
		{name: "confirmed failure", failure: errors.New("device rejected operation"), wantStatus: "failed"},
		{name: "unknown outcome", failure: temporal.NewTimeoutError(enums.TIMEOUT_TYPE_START_TO_CLOSE, nil), wantStatus: "unknown"},
		{name: "group consistency failure excludes complete group", failure: errors.New("inconsistent switch versions"), wantStatus: "failed", group: true},
		{name: "dependent infrastructure skipped after compute failure", failure: errors.New("compute decommission failed"), wantStatus: "failed", dependency: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			registerTaskUpdateActivities(env)
			var calls []string
			child := func(ctx temporalworkflow.Context, step operationrules.SequenceStep, target common.Target, info any, all map[devicetypes.ComponentType]common.Target) error {
				if !tc.group || step.Stage != 1 {
					require.Len(t, target.Identifiers, 1)
				} else {
					require.Equal(t, []string{"X", "Y", "Z"}, target.Identifiers)
				}
				id := strings.Join(target.Identifiers, ",")
				calls = append(calls, fmt.Sprintf("%d:%s", step.Stage, id))
				require.Equal(t, target, all[target.Type], "same-type checks must be isolated")
				if slices.Contains(target.Identifiers, "Y") && step.Stage == 1 {
					return tc.failure
				}
				return nil
			}
			if tc.observation {
				env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})
				env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
				env.OnActivity(activitypkg.NameGetPowerStatus, mock.Anything, mock.Anything).Return(func(_ context.Context, target common.Target) (map[string]operations.PowerStatus, error) {
					values := make(map[string]operations.PowerStatus)
					for _, id := range target.Identifiers {
						if id != "Y" {
							values[id] = operations.PowerStatusOn
						}
					}
					return values, nil
				})
			} else {
				env.RegisterWorkflowWithOptions(child, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})
			}
			type result struct {
				Report  *report.Report
				Failure string
			}
			env.ExecuteWorkflow(func(ctx temporalworkflow.Context) (result, error) {
				ctx = temporalworkflow.WithActivityOptions(ctx, temporalworkflow.ActivityOptions{StartToCloseTimeout: time.Minute})
				typ := devicetypes.ComponentTypeCompute
				targets := map[devicetypes.ComponentType]common.Target{typ: {Type: typ, Identifiers: []string{"X", "Y", "Z"}}}
				rule := &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{
					{Stage: 1, ComponentType: typ, MaxParallel: 1, MainOperation: operationrules.ActionConfig{Name: operationrules.ActionGetPowerStatus}},
					// Nonconsecutive stage numbers protect canonical report indexing.
					{Stage: 3, ComponentType: typ, MainOperation: operationrules.ActionConfig{Name: operationrules.ActionGetPowerStatus}},
				}}
				if tc.observation {
					rule.Steps[0].MainOperation = operationrules.ActionConfig{Name: operationrules.ActionVerifyPowerStatus, Timeout: time.Second, PollInterval: time.Second, Parameters: map[string]any{operationrules.ParamExpectedStatus: "on"}}
				}
				if tc.group {
					rule.Steps[0].MainOperation.Name = operationrules.ActionVerifyFirmwareConsistency
				}
				if tc.dependency {
					rule.Steps[1].ComponentType = devicetypes.ComponentTypePowerShelf
					targets[devicetypes.ComponentTypePowerShelf] = common.Target{Type: devicetypes.ComponentTypePowerShelf, Identifiers: []string{"P"}}
				}
				rep, err := executeRuleBasedOperation(ctx, uuid.New(), targets, targets, nil, rule, &taskcommon.TargetResolution{ExecutionMode: taskcommon.ExecutionModeBestEffort, Selected: 3, Submitted: 3})
				out := result{Report: rep}
				if err != nil {
					out.Failure = err.Error()
				}
				return out, nil
			})
			require.NoError(t, env.GetWorkflowError())
			var got result
			require.NoError(t, env.GetWorkflowResult(&got))
			require.NotEmpty(t, got.Failure, "continuation must retain the job failure")
			if tc.observation {
				first := got.Report.Stages[0].Steps[0].Targets
				later := got.Report.Stages[1].Steps[0].Targets
				require.Equal(t, "unknown", first[1].Status)
				require.Contains(t, first[1].Error, "timeout")
				require.Equal(t, "skipped", later[1].Status)
				require.Equal(t, "completed", later[0].Status)
				require.Equal(t, "completed", later[2].Status)
				return
			}
			if tc.group {
				require.Equal(t, []string{"1:X,Y,Z"}, calls, "group check must not split, and failed group must not reach stage 3")
			} else {
				require.Contains(t, calls, "1:Z", "a later batch must run after Y fails")
			}
			if tc.dependency {
				require.NotContains(t, calls, "3:P", "shared infrastructure depends on compute completion")
				require.Equal(t, "skipped", got.Report.Stages[1].Steps[0].Targets[0].Status)
				require.Contains(t, got.Report.Stages[1].Steps[0].Targets[0].Error, "dependency")
				return
			}
			if tc.group {
				for _, outcome := range got.Report.Stages[1].Steps[0].Targets {
					require.Equal(t, "skipped", outcome.Status)
				}
				return
			}
			require.Contains(t, calls, "3:X")
			require.Contains(t, calls, "3:Z")
			require.NotContains(t, calls, "3:Y")
			require.Equal(t, tc.wantStatus, got.Report.Stages[0].Steps[0].Targets[1].Status)
			require.NotEmpty(t, got.Report.Stages[0].Steps[0].Targets[1].Error)
			require.Equal(t, "skipped", got.Report.Stages[1].Steps[0].Targets[1].Status)
			require.Contains(t, got.Report.Stages[1].Steps[0].Targets[1].Error, "preceding stage 1")
			require.Equal(t, 2, got.Report.Stages[1].Steps[0].CompletedComponents)
		})
	}
}
