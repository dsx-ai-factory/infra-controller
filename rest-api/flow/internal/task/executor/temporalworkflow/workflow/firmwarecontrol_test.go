// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/worker"
	temporalworkflow "go.temporal.io/sdk/workflow"
	"google.golang.org/protobuf/encoding/protojson"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// mockFirmwareControl is a mock activity for starting firmware update
func mockFirmwareControl(ctx context.Context, target common.Target, info operations.FirmwareControlTaskInfo) error {
	return nil
}

// mockGetFirmwareStatus is a mock activity for getting firmware update status
func mockGetFirmwareStatus(ctx context.Context, target common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
	return &activitypkg.GetFirmwareStatusResult{
		Statuses: map[string]operations.FirmwareUpdateStatus{},
	}, nil
}

// createFirmwareTestRuleDef creates a minimal rule definition for firmware
// control tests. Single stage with all compute components running
// FirmwareControl, followed by a power recycle stage.
func createFirmwareTestRuleDef() *operationrules.RuleDefinition {
	return &operationrules.RuleDefinition{
		Version: "v1",
		Steps: []operationrules.SequenceStep{
			{
				ComponentType: devicetypes.ComponentTypeCompute,
				Stage:         1,
				MaxParallel:   0,
				Timeout:       30 * time.Minute,
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionFirmwareControl,
					Parameters: map[string]any{
						operationrules.ParamPollInterval: "1s",
						operationrules.ParamPollTimeout:  "1m",
					},
				},
			},
			{
				ComponentType: devicetypes.ComponentTypeCompute,
				Stage:         2,
				MaxParallel:   0,
				Timeout:       10 * time.Minute,
				PreOperation: []operationrules.ActionConfig{
					{
						Name: operationrules.ActionPowerControl,
						Parameters: map[string]any{
							operationrules.ParamOperation: "force_power_off",
						},
					},
					{
						Name: operationrules.ActionSleep,
						Parameters: map[string]any{
							operationrules.ParamDuration: 1 * time.Second,
						},
					},
				},
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionPowerControl,
					Parameters: map[string]any{
						operationrules.ParamOperation: "power_on",
					},
				},
				PostOperation: []operationrules.ActionConfig{
					{
						Name:         operationrules.ActionVerifyPowerStatus,
						Timeout:      5 * time.Second,
						PollInterval: 1 * time.Second,
						Parameters: map[string]any{
							operationrules.ParamExpectedStatus: "on",
						},
					},
				},
			},
		},
	}
}

// firmwareTestComponents creates WorkflowComponent slices for firmware tests.
// Each ID becomes a Compute component.
func firmwareTestComponents(
	externalIDs ...string,
) []task.WorkflowComponent {
	comps := make([]task.WorkflowComponent, len(externalIDs))
	for i, id := range externalIDs {
		comps[i] = task.WorkflowComponent{
			ComponentID: id,
			Type:        devicetypes.ComponentTypeCompute,
		}
	}
	return comps
}

func TestFirmwareControlWorkflow(t *testing.T) {
	now := time.Now()
	baseInfo := &operations.FirmwareControlTaskInfo{
		Operation: operations.FirmwareOperationUpgrade,
		StartTime: now.Unix(),
		EndTime:   now.Add(time.Hour * 2).Unix(),
	}
	baseReqInfo := task.ExecutionInfo{
		TaskID:         uuid.New(),
		Components:     firmwareTestComponents("comp1", "comp2"),
		RuleDefinition: createFirmwareTestRuleDef(),
	}
	// Exercise version selection after the parent passes operationInfo through
	// Temporal's child-workflow serialization, for every tray type in a rack.
	rackReqInfo := task.ExecutionInfo{
		TaskID:         uuid.New(),
		RuleDefinition: &operationrules.RuleDefinition{Version: "v1"},
	}
	for i, componentType := range []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
		devicetypes.ComponentTypePowerShelf,
	} {
		rackReqInfo.Components = append(rackReqInfo.Components, task.WorkflowComponent{
			ComponentID: devicetypes.ComponentTypeToString(componentType),
			Type:        componentType,
		})
		step := createFirmwareTestRuleDef().Steps[0]
		step.ComponentType = componentType
		step.Stage = i + 1
		rackReqInfo.RuleDefinition.Steps = append(rackReqInfo.RuleDefinition.Steps, step)
	}
	const sharedVersion = `{ "Id": "fw-default" }`
	layeredReqInfo := baseReqInfo
	layeredReqInfo.Components = append(firmwareTestComponents("comp1"), task.WorkflowComponent{
		ComponentID: "NVSwitch", Type: devicetypes.ComponentTypeNVSwitch,
	})
	layeredReqInfo.RuleDefinition = createFirmwareTestRuleDef()
	powerAction := operationrules.ActionConfig{
		Name:       operationrules.ActionPowerControl,
		Parameters: map[string]any{operationrules.ParamOperation: "power_on"},
	}
	layeredReqInfo.RuleDefinition.Steps[0].PreOperation = []operationrules.ActionConfig{powerAction}
	layeredReqInfo.RuleDefinition.Steps[0].PostOperation = []operationrules.ActionConfig{powerAction}
	switchStep := createFirmwareTestRuleDef().Steps[0]
	switchStep.ComponentType = devicetypes.ComponentTypeNVSwitch
	switchStep.Stage = 2
	switchStep.PreOperation = []operationrules.ActionConfig{{
		Name: operationrules.ActionVerifyReachability, Timeout: time.Minute, PollInterval: time.Second,
		Parameters: map[string]any{operationrules.ParamComponentTypes: []string{"compute"}},
	}}
	layeredReqInfo.RuleDefinition.Steps[1].Stage = 3
	layeredReqInfo.RuleDefinition.Steps = append(layeredReqInfo.RuleDefinition.Steps[:1],
		switchStep, layeredReqInfo.RuleDefinition.Steps[1])
	legacySelection := temporalworkflow.DefaultVersion

	testCases := map[string]struct {
		reqInfo       task.ExecutionInfo
		info          *operations.FirmwareControlTaskInfo
		activityError error
		expectError   bool
		versions      map[devicetypes.ComponentType]string
		selection     *temporalworkflow.Version
		computeStatus report.Status
		powerCalls    int
		history       string
	}{
		"legacy history schedules omitted component": {
			reqInfo: task.ExecutionInfo{
				TaskID: uuid.MustParse("00000000-0000-0000-0000-000000000001"), Components: firmwareTestComponents("comp1"),
				RuleDefinition: &operationrules.RuleDefinition{Version: "v1", Steps: []operationrules.SequenceStep{createFirmwareTestRuleDef().Steps[0]}},
			},
			info: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade, TargetVersion: `{"nvswitch":{"Id":"switch-fw"}}`,
			},
			history: `{"events":[
				{"eventId":"1","eventType":"EVENT_TYPE_WORKFLOW_EXECUTION_STARTED","workflowExecutionStartedEventAttributes":{"workflowType":{"name":"FirmwareControl"},"taskQueue":{"name":"firmware-replay"},"input":%s}},
				{"eventId":"2","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
				{"eventId":"3","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"2"}},
				{"eventId":"4","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"2","startedEventId":"3"}},
				{"eventId":"5","eventType":"EVENT_TYPE_ACTIVITY_TASK_SCHEDULED","activityTaskScheduledEventAttributes":{"activityId":"5","activityType":{"name":"UpdateTaskStatus"},"taskQueue":{"name":"firmware-replay"}}},
				{"eventId":"6","eventType":"EVENT_TYPE_ACTIVITY_TASK_STARTED","activityTaskStartedEventAttributes":{"scheduledEventId":"5"}},
				{"eventId":"7","eventType":"EVENT_TYPE_ACTIVITY_TASK_COMPLETED","activityTaskCompletedEventAttributes":{"scheduledEventId":"5","startedEventId":"6"}},
				{"eventId":"8","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
				{"eventId":"9","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"8"}},
				{"eventId":"10","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"8","startedEventId":"9"}},
				{"eventId":"11","eventType":"EVENT_TYPE_ACTIVITY_TASK_SCHEDULED","activityTaskScheduledEventAttributes":{"activityId":"11","activityType":{"name":"UpdateTaskReport"},"taskQueue":{"name":"firmware-replay"}}},
				{"eventId":"12","eventType":"EVENT_TYPE_ACTIVITY_TASK_STARTED","activityTaskStartedEventAttributes":{"scheduledEventId":"11"}},
				{"eventId":"13","eventType":"EVENT_TYPE_ACTIVITY_TASK_COMPLETED","activityTaskCompletedEventAttributes":{"scheduledEventId":"11","startedEventId":"12"}},
				{"eventId":"14","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
				{"eventId":"15","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"14"}},
				{"eventId":"16","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"14","startedEventId":"15"}},
				{"eventId":"17","eventType":"EVENT_TYPE_ACTIVITY_TASK_SCHEDULED","activityTaskScheduledEventAttributes":{"activityId":"17","activityType":{"name":"UpdateTaskReport"},"taskQueue":{"name":"firmware-replay"}}},
				{"eventId":"18","eventType":"EVENT_TYPE_ACTIVITY_TASK_STARTED","activityTaskStartedEventAttributes":{"scheduledEventId":"17"}},
				{"eventId":"19","eventType":"EVENT_TYPE_ACTIVITY_TASK_COMPLETED","activityTaskCompletedEventAttributes":{"scheduledEventId":"17","startedEventId":"18"}},
				{"eventId":"20","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
				{"eventId":"21","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"20"}},
				{"eventId":"22","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"20","startedEventId":"21"}},
				{"eventId":"23","eventType":"EVENT_TYPE_START_CHILD_WORKFLOW_EXECUTION_INITIATED","startChildWorkflowExecutionInitiatedEventAttributes":{"workflowId":"component-step-ReplayId-Compute","workflowType":{"name":"GenericComponentStepWorkflow"},"taskQueue":{"name":"firmware-replay"},"workflowTaskCompletedEventId":"22"}},
				{"eventId":"24","eventType":"EVENT_TYPE_CHILD_WORKFLOW_EXECUTION_STARTED","childWorkflowExecutionStartedEventAttributes":{"initiatedEventId":"23","workflowExecution":{"workflowId":"component-step-ReplayId-Compute","runId":"legacy-child"},"workflowType":{"name":"GenericComponentStepWorkflow"}}},
				{"eventId":"25","eventType":"EVENT_TYPE_WORKFLOW_TASK_SCHEDULED","workflowTaskScheduledEventAttributes":{}},
				{"eventId":"26","eventType":"EVENT_TYPE_WORKFLOW_TASK_STARTED","workflowTaskStartedEventAttributes":{"scheduledEventId":"25"}},
				{"eventId":"27","eventType":"EVENT_TYPE_WORKFLOW_TASK_COMPLETED","workflowTaskCompletedEventAttributes":{"scheduledEventId":"25","startedEventId":"26"}}
			]}`,
		},
		"success": {
			reqInfo:       baseReqInfo,
			info:          baseInfo,
			activityError: nil,
			expectError:   false,
		},
		"rack shares one firmware object across tray types": {
			reqInfo: rackReqInfo,
			info: &operations.FirmwareControlTaskInfo{
				Operation:     operations.FirmwareOperationUpgrade,
				TargetVersion: sharedVersion,
			},
			versions: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute:    sharedVersion,
				devicetypes.ComponentTypeNVSwitch:   sharedVersion,
				devicetypes.ComponentTypePowerShelf: sharedVersion,
			},
		},
		"empty rack version retains every tray type": {
			reqInfo: rackReqInfo,
			info:    &operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade},
			versions: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "", devicetypes.ComponentTypeNVSwitch: "", devicetypes.ComponentTypePowerShelf: "",
			},
		},
		"omitted layer skips all owned steps and preserves readiness targets": {
			reqInfo: layeredReqInfo,
			info: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade, TargetVersion: `{"nvswitch":{"Id":"switch-fw"}}`,
			},
			versions:      map[devicetypes.ComponentType]string{devicetypes.ComponentTypeNVSwitch: `{"Id":"switch-fw"}`},
			computeStatus: report.StatusSkipped,
		},
		"legacy selection retains omitted layer steps": {
			reqInfo: layeredReqInfo,
			info: &operations.FirmwareControlTaskInfo{
				Operation: operations.FirmwareOperationUpgrade, TargetVersion: `{"nvswitch":{"Id":"switch-fw"}}`,
			},
			versions: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "", devicetypes.ComponentTypeNVSwitch: `{"Id":"switch-fw"}`,
			},
			selection: &legacySelection, computeStatus: report.StatusCompleted, powerCalls: 4,
		},
		"rack selects each tray type's firmware object": {
			reqInfo: rackReqInfo,
			info: &operations.FirmwareControlTaskInfo{
				Operation:     operations.FirmwareOperationUpgrade,
				TargetVersion: `{"compute":{"Id":"compute-fw"},"nvswitch":{"Id":"switch-fw"},"powershelf":{"Id":"power-fw"}}`,
			},
			versions: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute:    `{"Id":"compute-fw"}`,
				devicetypes.ComponentTypeNVSwitch:   `{"Id":"switch-fw"}`,
				devicetypes.ComponentTypePowerShelf: `{"Id":"power-fw"}`,
			},
		},
		"activity fails": {
			reqInfo:       baseReqInfo,
			info:          baseInfo,
			activityError: errors.New("connection timeout"),
			expectError:   true,
		},
		"single machine success": {
			reqInfo: task.ExecutionInfo{
				TaskID:         uuid.New(),
				Components:     firmwareTestComponents("single-component"),
				RuleDefinition: createFirmwareTestRuleDef(),
			},
			info:          baseInfo,
			activityError: nil,
			expectError:   false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			if tc.history != "" {
				assertWorkflowHistoryReplay(t, "FirmwareControl", firmwareControl, tc.history, tc.reqInfo, tc.info)
				return
			}
			testSuite := &testsuite.WorkflowTestSuite{}
			env := testSuite.NewTestWorkflowEnvironment()

			env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})

			registerTaskUpdateActivities(env)
			env.RegisterActivityWithOptions(mockFirmwareControl, activity.RegisterOptions{
				Name: activitypkg.NameFirmwareControl,
			})
			env.RegisterActivityWithOptions(mockGetFirmwareStatus, activity.RegisterOptions{
				Name: activitypkg.NameGetFirmwareStatus,
			})
			env.RegisterActivityWithOptions(mockPowerControl, activity.RegisterOptions{
				Name: activitypkg.NamePowerControl,
			})
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{
				Name: activitypkg.NameGetPowerStatus,
			})
			if tc.selection != nil {
				env.OnGetVersion("firmware-layered-component-selection", temporalworkflow.DefaultVersion, 1).Return(*tc.selection)
			}

			if tc.versions == nil {
				env.OnActivity(mockFirmwareControl, mock.Anything, mock.Anything, mock.Anything).Return(tc.activityError)
			} else {
				for componentType, version := range tc.versions {
					target := common.Target{
						Type:           componentType,
						IdentifierType: common.IdentifierTypeManagerID,
						Identifiers:    []string{devicetypes.ComponentTypeToString(componentType)},
					}
					if tc.computeStatus != "" && componentType == devicetypes.ComponentTypeCompute {
						target.Identifiers = []string{"comp1"}
					}
					expectedInfo := *tc.info
					expectedInfo.TargetVersion = version
					env.OnActivity(mockFirmwareControl, mock.Anything, target, expectedInfo).Return(nil).Once()
				}
			}
			env.OnActivity(mockGetFirmwareStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
					statuses := make(map[string]operations.FirmwareUpdateStatus)
					for _, id := range target.Identifiers {
						statuses[id] = operations.FirmwareUpdateStatus{ComponentID: id, State: operations.FirmwareUpdateStateCompleted}
					}
					return &activitypkg.GetFirmwareStatusResult{Statuses: statuses}, nil
				})
			env.OnActivity(mockPowerControl, mock.Anything, mock.Anything, mock.Anything).Return(nil).Maybe()
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target) (map[string]operations.PowerStatus, error) {
					statuses := make(map[string]operations.PowerStatus, target.Len())
					for _, id := range target.Identifiers {
						statuses[id] = operations.PowerStatusOn
					}
					return statuses, nil
				}).Maybe()

			var finalReport json.RawMessage
			env.OnActivity(activitypkg.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(nil)
			env.OnActivity(activitypkg.NameUpdateTaskStatus, mock.Anything, mock.Anything).Return(nil).
				Run(func(args mock.Arguments) { finalReport = args.Get(1).(*task.TaskStatusUpdate).Report })
			env.ExecuteWorkflow(firmwareControl, tc.reqInfo, tc.info)

			assert.True(t, env.IsWorkflowCompleted())

			if tc.expectError {
				assert.Error(t, env.GetWorkflowError())
			} else {
				assert.NoError(t, env.GetWorkflowError())
			}
			if tc.versions != nil {
				env.AssertExpectations(t)
			}
			if tc.computeStatus != "" {
				require.NoError(t, env.GetWorkflowError())
				var result report.Report
				require.NoError(t, json.Unmarshal(finalReport, &result))
				require.Len(t, result.Stages, 3)
				for _, stage := range result.Stages {
					require.Len(t, stage.Steps, 1)
					for _, step := range stage.Steps {
						if step.ComponentType == "Compute" {
							assert.Equal(t, tc.computeStatus, step.Status)
							if tc.computeStatus == report.StatusSkipped {
								assert.Zero(t, step.TotalComponents)
							}
						} else {
							assert.Equal(t, report.StatusCompleted, step.Status)
						}
					}
				}
				env.AssertActivityNumberOfCalls(t, activitypkg.NamePowerControl, tc.powerCalls)
				env.AssertActivityCalled(t, activitypkg.NameGetPowerStatus, mock.Anything, common.Target{
					Type: devicetypes.ComponentTypeCompute, IdentifierType: common.IdentifierTypeManagerID, Identifiers: []string{"comp1"},
				})
			}
		})
	}
}

func assertWorkflowHistoryReplay(t *testing.T, workflowName string, workflow any, historyJSON string, args ...any) {
	t.Helper()
	input, err := converter.GetDefaultDataConverter().ToPayloads(args...)
	require.NoError(t, err)
	inputJSON, err := protojson.Marshal(input)
	require.NoError(t, err)
	var history historypb.History
	err = protojson.Unmarshal([]byte(fmt.Sprintf(historyJSON, inputJSON)), &history)
	require.NoError(t, err)
	replayer := worker.NewWorkflowReplayer()
	replayer.RegisterWorkflowWithOptions(workflow, temporalworkflow.RegisterOptions{Name: workflowName})
	require.NoError(t, replayer.ReplayWorkflowHistory(nil, &history))
}

func TestFirmwareControlWorkflowEmptyComponents(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	now := time.Now()
	// Empty Components slice — no components to operate on
	reqInfo := task.ExecutionInfo{
		TaskID:     uuid.New(),
		Components: []task.WorkflowComponent{},
	}
	info := &operations.FirmwareControlTaskInfo{
		Operation: operations.FirmwareOperationUpgrade,
		StartTime: now.Unix(),
		EndTime:   now.Add(time.Hour * 2).Unix(),
	}

	env.ExecuteWorkflow(firmwareControl, reqInfo, info)

	assert.True(t, env.IsWorkflowCompleted())
	assert.Error(t, env.GetWorkflowError()) // Should error because no components
}

func TestFirmwareControlWorkflowNoComponentIDs(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	now := time.Now()
	// nil Components slice — treated as no components
	reqInfo := task.ExecutionInfo{
		TaskID:         uuid.New(),
		Components:     nil,
		RuleDefinition: createFirmwareTestRuleDef(),
	}
	info := &operations.FirmwareControlTaskInfo{
		Operation: operations.FirmwareOperationUpgrade,
		StartTime: now.Unix(),
		EndTime:   now.Add(time.Hour * 2).Unix(),
	}

	env.ExecuteWorkflow(firmwareControl, reqInfo, info)

	assert.True(t, env.IsWorkflowCompleted())
	assert.Error(t, env.GetWorkflowError()) // Should error because no components
}
