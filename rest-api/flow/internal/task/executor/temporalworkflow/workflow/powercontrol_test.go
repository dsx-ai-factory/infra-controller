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

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskactivity "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

// mockPowerControl is a mock activity function for testing
func mockPowerControl(
	ctx context.Context,
	info taskcommon.ComponentInfo,
	pcInfo *operations.PowerControlTaskInfo,
) error {
	return nil
}

// mockGetPowerStatus is a mock activity function for testing power status verification.
// The actual return values are defined via env.OnActivity().Return() in each test case.
func mockGetPowerStatus(ctx context.Context, target common.Target) (map[string]operations.PowerStatus, error) {
	return nil, nil
}

// Helper function for creating test components
// - id: internal UUID (Flow database primary key)
// - name: human-readable name for DeviceInfo.Name
// - externalID: external component ID for activity calls
// - compType: component type
func newTestComponent(id uuid.UUID, name string, externalID string, compType devicetypes.ComponentType) *component.Component {
	return &component.Component{
		Type:        compType,
		ComponentID: externalID,
		Info: deviceinfo.DeviceInfo{
			ID:   id,
			Name: name,
		},
	}
}

// toWorkflowComponents converts a slice of *component.Component to
// []taskdef.WorkflowComponent for use in test ExecutionInfo structs.
func toWorkflowComponents(
	components []*component.Component,
) []taskdef.WorkflowComponent {
	if components == nil {
		return nil
	}
	result := make([]taskdef.WorkflowComponent, len(components))
	for i, c := range components {
		result[i] = taskdef.WorkflowComponent{
			Type:        c.Type,
			ComponentID: c.ComponentID,
		}
	}
	return result
}

// Helper function to create a default rule definition for power operations
func createDefaultPowerRuleDef(op operations.PowerOperation) *operationrules.RuleDefinition {
	switch op {
	case operations.PowerOperationPowerOn, operations.PowerOperationForcePowerOn:
		return &operationrules.RuleDefinition{
			Version: "v1",
			Steps: []operationrules.SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         1,
					MaxParallel:   1,
					DelayAfter:    30 * time.Second,
					Timeout:       10 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   1,
					DelayAfter:    15 * time.Second,
					Timeout:       15 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         3,
					MaxParallel:   1,
					Timeout:       20 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
			},
		}
	case operations.PowerOperationPowerOff, operations.PowerOperationForcePowerOff:
		return &operationrules.RuleDefinition{
			Version: "v1",
			Steps: []operationrules.SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   1,
					DelayAfter:    10 * time.Second,
					Timeout:       20 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypeNVSwitch,
					Stage:         2,
					MaxParallel:   1,
					DelayAfter:    5 * time.Second,
					Timeout:       15 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
				{
					ComponentType: devicetypes.ComponentTypePowerShelf,
					Stage:         3,
					MaxParallel:   1,
					Timeout:       10 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
			},
		}
	case operations.PowerOperationRestart, operations.PowerOperationForceRestart:
		return &operationrules.RuleDefinition{
			Version: "v1",
			Steps: []operationrules.SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					Stage:         1,
					MaxParallel:   1,
					Timeout:       20 * time.Minute,
					MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
				},
			},
		}
	case operations.PowerOperationWarmReset, operations.PowerOperationColdReset:
		// Return empty steps for unsupported operations
		return &operationrules.RuleDefinition{
			Version: "v1",
			Steps:   []operationrules.SequenceStep{},
		}
	default:
		// Default minimal rule
		return &operationrules.RuleDefinition{
			Version: "v1",
			Steps:   []operationrules.SequenceStep{},
		}
	}
}

func TestPowerControlWorkflow(t *testing.T) {
	computeID1 := uuid.New()
	computeID2 := uuid.New()
	powershelfID := uuid.New()
	nvswitchID := uuid.New()

	// Full set of components (PowerShelf, NVSwitch, Compute)
	fullComponents := []*component.Component{
		newTestComponent(powershelfID, "powershelf-1", "ext-powershelf-1", devicetypes.ComponentTypePowerShelf),
		newTestComponent(nvswitchID, "nvswitch-1", "ext-nvswitch-1", devicetypes.ComponentTypeNVSwitch),
		newTestComponent(computeID1, "compute-1", "ext-compute-1", devicetypes.ComponentTypeCompute),
		newTestComponent(computeID2, "compute-2", "ext-compute-2", devicetypes.ComponentTypeCompute),
	}

	// Compute-only components
	computeOnlyComponents := []*component.Component{
		newTestComponent(computeID1, "compute-1", "ext-compute-1", devicetypes.ComponentTypeCompute),
		newTestComponent(computeID2, "compute-2", "ext-compute-2", devicetypes.ComponentTypeCompute),
	}

	testCases := map[string]struct {
		components    []*component.Component
		op            operations.PowerOperation
		activityError error
		expectError   bool
		errorContains string
	}{
		"power on full components success": {
			components:    fullComponents,
			op:            operations.PowerOperationPowerOn,
			activityError: nil,
			expectError:   false,
		},
		"power off full components success": {
			components:    fullComponents,
			op:            operations.PowerOperationPowerOff,
			activityError: nil,
			expectError:   false,
		},
		"force power on success": {
			components:    fullComponents,
			op:            operations.PowerOperationForcePowerOn,
			activityError: nil,
			expectError:   false,
		},
		"force power off success": {
			components:    fullComponents,
			op:            operations.PowerOperationForcePowerOff,
			activityError: nil,
			expectError:   false,
		},
		"power on compute only": {
			components:    computeOnlyComponents,
			op:            operations.PowerOperationPowerOn,
			activityError: nil,
			expectError:   false,
		},
		"restart success": {
			components:    computeOnlyComponents,
			op:            operations.PowerOperationRestart,
			activityError: nil,
			expectError:   false,
		},
		"force restart success": {
			components:    computeOnlyComponents,
			op:            operations.PowerOperationForceRestart,
			activityError: nil,
			expectError:   false,
		},
		"warm reset not supported": {
			components:    fullComponents,
			op:            operations.PowerOperationWarmReset,
			activityError: nil,
			expectError:   true,
			errorContains: "rule definition has no steps",
		},
		"cold reset not supported": {
			components:    fullComponents,
			op:            operations.PowerOperationColdReset,
			activityError: nil,
			expectError:   true,
			errorContains: "rule definition has no steps",
		},
		"activity failure returns error": {
			components:    computeOnlyComponents,
			op:            operations.PowerOperationPowerOn,
			activityError: errors.New("BMC connection failed"),
			expectError:   true,
			errorContains: "BMC connection failed",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			testSuite := &testsuite.WorkflowTestSuite{}
			env := testSuite.NewTestWorkflowEnvironment()

			registerTaskUpdateActivities(env)
			env.RegisterActivityWithOptions(mockPowerControl, activity.RegisterOptions{
				Name: taskactivity.NamePowerControl,
			})
			env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{
				Name: taskactivity.NameGetPowerStatus,
			})
			// Register the child workflow needed for rule-based execution
			env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})

			env.OnActivity(taskactivity.NamePowerControl, mock.Anything, mock.Anything, mock.Anything).Return(tc.activityError)

			// Track call count for restart operations which need Off then On
			callCount := 0
			// Count unique component types to know when off phase ends
			numComponentTypes := 0
			if tc.components != nil {
				typesSeen := make(map[devicetypes.ComponentType]bool)
				for _, c := range tc.components {
					typesSeen[c.Type] = true
				}
				numComponentTypes = len(typesSeen)
			}

			env.OnActivity(taskactivity.NameGetPowerStatus, mock.Anything, mock.Anything).Return(
				func(ctx context.Context, target common.Target) (map[string]operations.PowerStatus, error) {
					callCount++
					result := make(map[string]operations.PowerStatus)

					// Determine expected status based on operation and call sequence
					var expectedStatus operations.PowerStatus
					switch tc.op {
					case operations.PowerOperationPowerOff, operations.PowerOperationForcePowerOff:
						expectedStatus = operations.PowerStatusOff
					case operations.PowerOperationRestart, operations.PowerOperationForceRestart:
						// Restart: first phase is off (1 call per component type), then on
						// Off phase ends after we've verified each component type once
						if callCount <= numComponentTypes {
							expectedStatus = operations.PowerStatusOff
						} else {
							expectedStatus = operations.PowerStatusOn
						}
					default:
						expectedStatus = operations.PowerStatusOn
					}

					for _, componentID := range target.Identifiers {
						result[componentID] = expectedStatus
					}
					return result, nil
				},
			)

			expectTaskUpdateActivities(env)

			info := &operations.PowerControlTaskInfo{Operation: tc.op}
			reqInfo := taskdef.ExecutionInfo{
				TaskID:         uuid.New(),
				Components:     toWorkflowComponents(tc.components),
				RuleDefinition: createDefaultPowerRuleDef(tc.op),
			}

			env.ExecuteWorkflow(powerControl, reqInfo, info)

			assert.True(t, env.IsWorkflowCompleted())

			wfErr := env.GetWorkflowError()
			if tc.expectError {
				assert.Error(t, wfErr)
				if tc.errorContains != "" && wfErr != nil {
					assert.Contains(t, wfErr.Error(), tc.errorContains)
				}
			} else {
				assert.NoError(t, wfErr)
			}
		})
	}
}
