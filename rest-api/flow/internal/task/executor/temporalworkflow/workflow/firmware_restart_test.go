// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func testNVOSRestartWorkflow(t *testing.T) {
	for _, tc := range []struct {
		name         string
		subTargets   []string
		version      string
		legacy       bool
		legacyWorker bool
		flashFails   bool
		powerFails   bool
		staysOn      bool
		wantRestart  bool
	}{
		{name: "NVOS waits for update then restarts all compute", subTargets: []string{"nvos"}, wantRestart: true},
		{name: "whole bundle restarts compute", wantRestart: true},
		{name: "BMC-only never restarts compute", subTargets: []string{"bmc"}},
		{name: "omitted switch version layer never restarts", version: `{"compute":{"Id":"compute-fw"}}`},
		{name: "predecessor history has no new commands", legacy: true},
		{name: "old worker history with new input does not add commands", legacyWorker: true},
		{name: "firmware failure prevents reboot", subTargets: []string{"nvos"}, flashFails: true},
		{name: "restart rejection fails task", subTargets: []string{"nvos"}, powerFails: true, wantRestart: true},
		{name: "shutdown must be observed before power on", subTargets: []string{"nvos"}, staysOn: true, wantRestart: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info := &operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade,
				TargetVersion: tc.version, SubTargets: tc.subTargets, OverrideReadinessCheck: true}
			rule := createFirmwareTestRuleDef()
			rule.Steps = rule.Steps[:1]
			rule.Steps[0].ComponentType = devicetypes.ComponentTypeNVSwitch
			rule.Steps[0].Stage = 8 // continuation must follow sparse/custom stages
			req := task.ExecutionInfo{TaskID: uuid.New(), RuleDefinition: rule,
				Components:                []task.WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "switch-1"}},
				FirmwareRestartComponents: firmwareTestComponents("compute-1", "compute-2")}
			if tc.legacy {
				req.FirmwareRestartComponents = nil
			}
			env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
			env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})
			registerTaskUpdateActivities(env)
			if tc.legacyWorker {
				env.OnGetVersion(nvosComputeRestartChangeID, temporalworkflow.DefaultVersion, 1).Return(temporalworkflow.DefaultVersion)
			}
			env.RegisterActivityWithOptions(mockFirmwareControl, activity.RegisterOptions{Name: activitypkg.NameFirmwareControl})
			env.RegisterActivityWithOptions(mockGetFirmwareStatus, activity.RegisterOptions{Name: activitypkg.NameGetFirmwareStatus})
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			// The callback signature matches the production activity's serialized target.
			powerActivity := func(context.Context, common.Target, operations.PowerControlTaskInfo) error { return nil }
			env.RegisterActivityWithOptions(powerActivity, activity.RegisterOptions{Name: activitypkg.NamePowerControl})
			env.OnActivity(mockFirmwareControl, mock.Anything, mock.Anything, mock.Anything).Return(nil).Maybe()
			var mu sync.Mutex
			var events []string
			polls := 0
			state := operations.PowerStatusOn
			env.OnActivity(mockGetFirmwareStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
					mu.Lock()
					defer mu.Unlock()
					require.Equal(t, []string{"switch-1"}, target.Identifiers)
					polls++
					status := operations.FirmwareUpdateStateVerifying
					if polls >= 2 {
						status = operations.FirmwareUpdateStateCompleted
						events = append(events, "firmware-completed")
					}
					if tc.flashFails {
						status = operations.FirmwareUpdateStateFailed
					}
					return &activitypkg.GetFirmwareStatusResult{Statuses: map[string]operations.FirmwareUpdateStatus{
						"switch-1": {ComponentID: "switch-1", State: status}}}, nil
				}).Maybe()
			env.OnActivity(activitypkg.NamePowerControl, mock.Anything, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target, power operations.PowerControlTaskInfo) error {
					mu.Lock()
					defer mu.Unlock()
					require.Equal(t, []string{"compute-1", "compute-2"}, target.Identifiers)
					require.True(t, power.OverrideReadinessCheck)
					require.GreaterOrEqual(t, polls, 2)
					events = append(events, power.Operation.CodeString())
					if tc.powerFails {
						return temporal.NewNonRetryableApplicationError("restart rejected", "test", nil)
					}
					if power.Operation == operations.PowerOperationPowerOff && !tc.staysOn {
						state = operations.PowerStatusOff
					}
					if power.Operation == operations.PowerOperationPowerOn {
						state = operations.PowerStatusOn
					}
					return nil
				}).Maybe()
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target) (map[string]operations.PowerStatus, error) {
					mu.Lock()
					defer mu.Unlock()
					return map[string]operations.PowerStatus{"compute-1": state, "compute-2": state}, nil
				}).Maybe()
			var final *task.TaskStatusUpdate
			env.OnActivity(activitypkg.NameUpdateTaskStatus, mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
				mu.Lock()
				defer mu.Unlock()
				final = args.Get(1).(*task.TaskStatusUpdate)
			}).Return(nil)
			env.OnActivity(activitypkg.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(nil)
			env.ExecuteWorkflow(firmwareControl, req, info)
			mu.Lock()
			defer mu.Unlock()
			if tc.flashFails || tc.powerFails || tc.staysOn {
				require.Error(t, env.GetWorkflowError())
			} else {
				require.NoError(t, env.GetWorkflowError())
			}
			require.NotNil(t, final)
			rep, err := report.Unmarshal(final.Report)
			require.NoError(t, err)
			if tc.flashFails {
				require.Empty(t, events)
				require.Len(t, rep.Stages, 3)
				require.Equal(t, report.StatusFailed, rep.Stages[0].Status)
				require.Equal(t, report.StatusPending, rep.Stages[1].Status)
				require.Equal(t, report.StatusPending, rep.Stages[2].Status)
				return
			}
			if tc.wantRestart {
				require.Len(t, rep.Stages, 3)
				require.Equal(t, 9, rep.Stages[1].Number)
				require.Equal(t, 2, rep.Stages[1].Steps[0].TotalComponents)
				require.Equal(t, 2, rep.Stages[2].Steps[0].TotalComponents)
				if !tc.powerFails && !tc.staysOn {
					require.Equal(t, []string{"firmware-completed", "power_off", "power_on"}, events)
					require.Equal(t, report.StatusCompleted, rep.Stages[2].Status)
				} else {
					require.Equal(t, []string{"firmware-completed", "power_off"}, events)
					require.Equal(t, report.StatusFailed, rep.Stages[1].Status)
				}
			} else {
				require.NotContains(t, events, "power_off")
				require.Len(t, rep.Stages, 1)
			}
			// Parent timeout includes the full restart continuation, not only flashing.
			withoutRestart := req
			withoutRestart.FirmwareRestartComponents = nil
			if tc.wantRestart {
				require.Greater(t, ExecutionTimeout(&req, info, time.Hour), ExecutionTimeout(&withoutRestart, info, time.Hour))
			}
		})
	}
}
