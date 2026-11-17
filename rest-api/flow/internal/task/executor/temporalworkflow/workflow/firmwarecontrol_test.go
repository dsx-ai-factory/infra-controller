// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/temporal"
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
	parallelReqInfo := baseReqInfo
	parallelReqInfo.Components = append(firmwareTestComponents("comp1", "comp2"), task.WorkflowComponent{
		ComponentID: "ps1", Type: devicetypes.ComponentTypePowerShelf,
	})
	computeStep := createFirmwareTestRuleDef().Steps[0]
	powerShelfStep := computeStep
	powerShelfStep.ComponentType = devicetypes.ComponentTypePowerShelf
	parallelReqInfo.RuleDefinition = &operationrules.RuleDefinition{
		Version: "v1", Steps: []operationrules.SequenceStep{computeStep, powerShelfStep},
	}
	postFailureReqInfo := baseReqInfo
	postFailureReqInfo.RuleDefinition = createFirmwareTestRuleDef()
	postFailureReqInfo.RuleDefinition.Steps = postFailureReqInfo.RuleDefinition.Steps[:1]
	postFailureReqInfo.RuleDefinition.Steps[0].PostOperation = []operationrules.ActionConfig{{
		Name:       operationrules.ActionPowerControl,
		Parameters: map[string]any{operationrules.ParamOperation: "power_on"},
	}}

	testCases := map[string]struct {
		reqInfo         task.ExecutionInfo
		info            *operations.FirmwareControlTaskInfo
		activityError   error
		expectError     bool
		versions        map[devicetypes.ComponentType]string
		selection       *temporalworkflow.Version
		computeStatus   report.Status
		powerCalls      int
		history         string
		statusResponses []map[string]operations.FirmwareUpdateStatus
		wantProgress    []firmwareStepProgress
		checkProgress   bool
		legacyProgress  bool
		parallelFailure bool
		wantFinal       []firmwareStepProgress
		postFailure     bool
		wantStepErrors  map[string]string
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
		"persists firmware component progress": {
			reqInfo: baseReqInfo,
			info:    baseInfo,
			statusResponses: []map[string]operations.FirmwareUpdateStatus{
				{
					"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
					"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateVerifying),
				},
				{
					"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
					"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateCompleted),
				},
			},
			wantProgress: []firmwareStepProgress{
				{
					StageNumber:         1,
					ComponentType:       "Compute",
					CompletedComponents: 1,
				},
				{
					StageNumber:         1,
					ComponentType:       "Compute",
					CompletedComponents: 2,
				},
			},
			checkProgress: true,
		},
		"waits for compute progress after parallel powershelf failure": {
			reqInfo:         parallelReqInfo,
			info:            baseInfo,
			expectError:     true,
			parallelFailure: true,
			wantStepErrors:  map[string]string{"Compute": "", "PowerShelf": "ps1"},
			statusResponses: []map[string]operations.FirmwareUpdateStatus{
				{
					"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
					"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateVerifying),
				},
				{
					"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
					"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateCompleted),
				},
			},
			wantFinal: []firmwareStepProgress{
				{StageNumber: 1, ComponentType: "Compute", CompletedComponents: 2},
				{StageNumber: 1, ComponentType: "PowerShelf", FailedComponents: 1},
			},
		},
		"parallel failures retain their own errors": {
			reqInfo: parallelReqInfo, info: baseInfo, expectError: true, parallelFailure: true,
			statusResponses: []map[string]operations.FirmwareUpdateStatus{{
				"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
				"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateFailed),
			}},
			wantFinal: []firmwareStepProgress{
				{StageNumber: 1, ComponentType: "Compute", CompletedComponents: 1, FailedComponents: 1},
				{StageNumber: 1, ComponentType: "PowerShelf", FailedComponents: 1},
			},
			wantStepErrors: map[string]string{"Compute": "comp2", "PowerShelf": "ps1"},
		},
		"post-operation failure retains completed firmware counters": {
			reqInfo: postFailureReqInfo, info: baseInfo, expectError: true, postFailure: true,
			wantFinal:      []firmwareStepProgress{{StageNumber: 1, ComponentType: "Compute", CompletedComponents: 2}},
			wantStepErrors: map[string]string{"Compute": "post-operation failed"},
		},
		"timeout leaves unresolved components uncounted": {
			reqInfo: baseReqInfo, info: baseInfo, expectError: true,
			statusResponses: []map[string]operations.FirmwareUpdateStatus{{
				"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
				"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateVerifying),
			}},
			wantFinal:      []firmwareStepProgress{{StageNumber: 1, ComponentType: "Compute", CompletedComponents: 1}},
			wantStepErrors: map[string]string{"Compute": "timed out"},
		},
		"persists terminal counters when firmware fails": {
			reqInfo:     baseReqInfo,
			info:        baseInfo,
			expectError: true,
			statusResponses: []map[string]operations.FirmwareUpdateStatus{
				{
					"comp1": firmwareStatus("comp1", operations.FirmwareUpdateStateCompleted),
					"comp2": firmwareStatus("comp2", operations.FirmwareUpdateStateFailed),
				},
			},
			wantProgress: []firmwareStepProgress{
				{
					StageNumber:         1,
					ComponentType:       "Compute",
					CompletedComponents: 1,
					FailedComponents:    1,
				},
			},
			checkProgress: true,
		},
		"legacy history retains reports without component progress": {
			reqInfo:        baseReqInfo,
			info:           baseInfo,
			checkProgress:  true,
			legacyProgress: true,
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
			if tc.legacyProgress {
				env.OnGetVersion(
					firmwareReportCountersChangeID,
					temporalworkflow.DefaultVersion,
					temporalworkflow.Version(1),
				).Return(temporalworkflow.DefaultVersion).Twice()
			}

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
			var statusMu sync.Mutex
			statusCall := 0
			env.OnActivity(mockGetFirmwareStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
					if tc.parallelFailure && target.Type == devicetypes.ComponentTypePowerShelf {
						return &activitypkg.GetFirmwareStatusResult{
							Statuses: map[string]operations.FirmwareUpdateStatus{
								"ps1": firmwareStatus("ps1", operations.FirmwareUpdateStateFailed),
							},
						}, nil
					}
					if len(tc.statusResponses) > 0 {
						statusMu.Lock()
						defer statusMu.Unlock()
						responseIndex := min(statusCall, len(tc.statusResponses)-1)
						statusCall++
						return &activitypkg.GetFirmwareStatusResult{
							Statuses: tc.statusResponses[responseIndex],
						}, nil
					}
					statuses := make(map[string]operations.FirmwareUpdateStatus)
					for _, id := range target.Identifiers {
						statuses[id] = operations.FirmwareUpdateStatus{ComponentID: id, State: operations.FirmwareUpdateStateCompleted}
					}
					return &activitypkg.GetFirmwareStatusResult{Statuses: statuses}, nil
				})
			var powerErr error
			if tc.postFailure {
				powerErr = temporal.NewNonRetryableApplicationError("post-operation power failure", "test", nil)
			}
			env.OnActivity(mockPowerControl, mock.Anything, mock.Anything, mock.Anything).Return(powerErr).Maybe()
			env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, target common.Target) (map[string]operations.PowerStatus, error) {
					statuses := make(map[string]operations.PowerStatus, target.Len())
					for _, id := range target.Identifiers {
						statuses[id] = operations.PowerStatusOn
					}
					return statuses, nil
				}).Maybe()

			var finalReport json.RawMessage
			var reportMu sync.Mutex
			var reportSnapshots [][]byte
			env.OnActivity(activitypkg.NameUpdateTaskStatus, mock.Anything, mock.Anything).Return(
				func(_ context.Context, update *task.TaskStatusUpdate) error {
					if len(update.Report) == 0 {
						return nil
					}
					reportMu.Lock()
					defer reportMu.Unlock()
					finalReport = append([]byte(nil), update.Report...)
					reportSnapshots = append(reportSnapshots, append([]byte(nil), update.Report...))
					return nil
				},
			)
			env.OnActivity(activitypkg.NameUpdateTaskReport, mock.Anything, mock.Anything).Return(
				func(_ context.Context, update *task.TaskReportUpdate) error {
					reportMu.Lock()
					defer reportMu.Unlock()
					reportSnapshots = append(reportSnapshots, append([]byte(nil), update.Report...))
					return nil
				},
			)
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
			if tc.checkProgress || tc.wantFinal != nil {
				rep, err := report.Unmarshal(finalReport)
				require.NoError(t, err)
				require.NotNil(t, rep)
				wantStatus := report.StatusCompleted
				if tc.expectError {
					wantStatus = report.StatusFailed
				}
				require.NotEmpty(t, rep.Stages)
				require.Equal(t, wantStatus, rep.Stages[0].Status)
				wantFinal := tc.wantFinal
				if wantFinal == nil {
					last := firmwareStepProgress{StageNumber: 1, ComponentType: "Compute"}
					if len(tc.wantProgress) > 0 {
						last = tc.wantProgress[len(tc.wantProgress)-1]
					}
					wantFinal = []firmwareStepProgress{last}
				}
				var gotFinal []firmwareStepProgress
				for _, step := range rep.Stages[0].Steps {
					if wantError, ok := tc.wantStepErrors[step.ComponentType]; ok {
						require.NotEmpty(t, step.StartedAt)
						require.NotEmpty(t, step.FinishedAt)
						if wantError == "" {
							require.Equal(t, report.StatusCompleted, step.Status)
							require.Empty(t, step.Error)
						} else {
							require.Equal(t, report.StatusFailed, step.Status)
							require.Contains(t, step.Error, wantError)
						}
					}
					gotFinal = append(gotFinal, firmwareStepProgress{
						StageNumber: rep.Stages[0].Number, ComponentType: step.ComponentType,
						CompletedComponents: step.CompletedComponents, FailedComponents: step.FailedComponents,
					})
				}
				require.ElementsMatch(t, wantFinal, gotFinal)
			}
			if tc.checkProgress {
				reportMu.Lock()
				snapshots := append([][]byte(nil), reportSnapshots...)
				reportMu.Unlock()

				var gotProgress []firmwareStepProgress
				for _, raw := range snapshots {
					rep, err := report.Unmarshal(raw)
					require.NoError(t, err)
					for _, stage := range rep.Stages {
						if stage.Number != 1 {
							continue
						}
						for _, step := range stage.Steps {
							progress := firmwareStepProgress{
								StageNumber:         stage.Number,
								ComponentType:       step.ComponentType,
								CompletedComponents: step.CompletedComponents,
								FailedComponents:    step.FailedComponents,
							}
							if progress.CompletedComponents == 0 && progress.FailedComponents == 0 {
								continue
							}
							if len(gotProgress) == 0 || gotProgress[len(gotProgress)-1] != progress {
								gotProgress = append(gotProgress, progress)
							}
						}
					}
				}
				require.Equal(t, tc.wantProgress, gotProgress)
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
