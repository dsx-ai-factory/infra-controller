// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
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

	testCases := map[string]struct {
		reqInfo       task.ExecutionInfo
		info          *operations.FirmwareControlTaskInfo
		activityError error
		expectError   bool
		versions      map[devicetypes.ComponentType]string
	}{
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

			if tc.versions == nil {
				env.OnActivity(mockFirmwareControl, mock.Anything, mock.Anything, mock.Anything).Return(tc.activityError)
			} else {
				for componentType, version := range tc.versions {
					target := common.Target{
						Type:           componentType,
						IdentifierType: common.IdentifierTypeManagerID,
						Identifiers:    []string{devicetypes.ComponentTypeToString(componentType)},
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
			if tc.versions == nil {
				env.OnActivity(mockPowerControl, mock.Anything, mock.Anything, mock.Anything).Return(nil)
				powerStatuses := make(map[string]operations.PowerStatus, len(tc.reqInfo.Components))
				for _, component := range tc.reqInfo.Components {
					powerStatuses[component.ComponentID] = operations.PowerStatusOn
				}
				env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(
					powerStatuses, nil)
			}

			expectTaskUpdateActivities(env)
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
		})
	}
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
