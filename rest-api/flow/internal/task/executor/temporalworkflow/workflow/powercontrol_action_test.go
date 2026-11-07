// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
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
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

// TestPowerControlWorkflow_GracefulWithVerification tests graceful power
// operations with verification actions
func TestPowerControlWorkflow_GracefulWithVerification(t *testing.T) {
	testCases := []struct {
		name      string
		operation operations.PowerOperation
		ruleDef   *operationrules.RuleDefinition
	}{
		{
			name:      "power on with verification",
			operation: operations.PowerOperationPowerOn,
			ruleDef: &operationrules.RuleDefinition{
				Version: "v1",
				Steps: []operationrules.SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						MaxParallel:   0,
						Timeout:       10 * time.Minute,
						MainOperation: operationrules.ActionConfig{
							Name: operationrules.ActionPowerControl,
						},
						PostOperation: []operationrules.ActionConfig{
							{
								Name:         operationrules.ActionVerifyPowerStatus,
								Timeout:      15 * time.Second,
								PollInterval: 5 * time.Second,
								Parameters: map[string]any{
									operationrules.ParamExpectedStatus: "on",
								},
							},
						},
					},
				},
			},
		},
		{
			name:      "power off with verification",
			operation: operations.PowerOperationPowerOff,
			ruleDef: &operationrules.RuleDefinition{
				Version: "v1",
				Steps: []operationrules.SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						MaxParallel:   0,
						Timeout:       10 * time.Minute,
						MainOperation: operationrules.ActionConfig{
							Name: operationrules.ActionPowerControl,
						},
						PostOperation: []operationrules.ActionConfig{
							{
								Name:         operationrules.ActionVerifyPowerStatus,
								Timeout:      15 * time.Second,
								PollInterval: 5 * time.Second,
								Parameters: map[string]any{
									operationrules.ParamExpectedStatus: "off",
								},
							},
						},
					},
				},
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			testSuite := &testsuite.WorkflowTestSuite{}
			env := testSuite.NewTestWorkflowEnvironment()

			// Register activities
			env.RegisterActivityWithOptions(mockPowerControl,
				activity.RegisterOptions{Name: activitypkg.NamePowerControl})
			env.RegisterActivityWithOptions(mockGetPowerStatus,
				activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			registerTaskUpdateActivities(env)

			// Mock activity responses
			env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
				mock.Anything).Return(nil)

			// Mock GetPowerStatus to return expected status
			expectedStatus := operations.PowerStatusOn
			if tc.operation == operations.PowerOperationPowerOff {
				expectedStatus = operations.PowerStatusOff
			}
			env.OnActivity(mockGetPowerStatus, mock.Anything,
				mock.Anything).Return(map[string]operations.PowerStatus{
				"ext-compute-1": expectedStatus,
			}, nil)

			// Create test components
			components := []*component.Component{
				newTestComponent(uuid.New(), "compute-1", "ext-compute-1",
					devicetypes.ComponentTypeCompute),
			}

			info := &operations.PowerControlTaskInfo{
				Operation: tc.operation,
			}

			reqInfo := taskdef.ExecutionInfo{
				TaskID:         uuid.New(),
				Components:     toWorkflowComponents(components),
				RuleDefinition: tc.ruleDef,
			}

			// Register child workflow
			env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})

			// Execute workflow
			expectTaskUpdateActivities(env)
			env.ExecuteWorkflow(powerControl, reqInfo, info)

			// Verify workflow completed successfully
			assert.True(t, env.IsWorkflowCompleted())
			assert.NoError(t, env.GetWorkflowError())
		})
	}
}

// TestPowerControlWorkflow_ForcefulWithFinalVerification tests forceful
// operations with final verification stage
func TestPowerControlWorkflow_ForcefulWithFinalVerification(t *testing.T) {
	testCases := []struct {
		name      string
		operation operations.PowerOperation
		ruleDef   *operationrules.RuleDefinition
	}{
		{
			name:      "force power on with final verification",
			operation: operations.PowerOperationForcePowerOn,
			ruleDef: &operationrules.RuleDefinition{
				Version: "v1",
				Steps: []operationrules.SequenceStep{
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         1,
						MaxParallel:   0,
						Timeout:       10 * time.Minute,
						MainOperation: operationrules.ActionConfig{
							Name: operationrules.ActionPowerControl,
						},
						PostOperation: []operationrules.ActionConfig{
							{
								Name: operationrules.ActionSleep,
								Parameters: map[string]any{
									operationrules.ParamDuration: 5 * time.Second,
								},
							},
						},
					},
					// Final verification stage
					{
						ComponentType: devicetypes.ComponentTypeCompute,
						Stage:         2,
						MaxParallel:   0,
						Timeout:       2 * time.Minute,
						MainOperation: operationrules.ActionConfig{
							Name:         operationrules.ActionVerifyPowerStatus,
							Timeout:      1 * time.Minute,
							PollInterval: 5 * time.Second,
							Parameters: map[string]any{
								operationrules.ParamExpectedStatus: "on",
							},
						},
					},
				},
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			testSuite := &testsuite.WorkflowTestSuite{}
			env := testSuite.NewTestWorkflowEnvironment()

			// Register activities
			env.RegisterActivityWithOptions(mockPowerControl,
				activity.RegisterOptions{Name: activitypkg.NamePowerControl})
			env.RegisterActivityWithOptions(mockGetPowerStatus,
				activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
			registerTaskUpdateActivities(env)

			// Mock activity responses
			env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
				mock.Anything).Return(nil)
			env.OnActivity(mockGetPowerStatus, mock.Anything,
				mock.Anything).Return(map[string]operations.PowerStatus{
				"ext-compute-1": operations.PowerStatusOn,
			}, nil)

			// Create test components
			components := []*component.Component{
				newTestComponent(uuid.New(), "compute-1", "ext-compute-1",
					devicetypes.ComponentTypeCompute),
			}

			info := &operations.PowerControlTaskInfo{
				Operation: tc.operation,
			}

			reqInfo := taskdef.ExecutionInfo{
				TaskID:         uuid.New(),
				Components:     toWorkflowComponents(components),
				RuleDefinition: tc.ruleDef,
			}

			// Register child workflow
			env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})

			// Execute workflow
			expectTaskUpdateActivities(env)
			env.ExecuteWorkflow(powerControl, reqInfo, info)

			// Verify workflow completed successfully
			assert.True(t, env.IsWorkflowCompleted())
			assert.NoError(t, env.GetWorkflowError())
		})
	}
}

// TestPowerControlWorkflow_CompositeVerification tests power on with
// both power status and reachability verification
func TestPowerControlWorkflow_CompositeVerification(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	// Mock verification activity
	mockVerifyReachability := func(
		ctx context.Context,
		allTargets map[devicetypes.ComponentType]common.Target,
		componentTypes []string,
		timeout time.Duration,
		pollInterval time.Duration,
	) error {
		return nil
	}

	// Register activities
	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})
	env.RegisterActivityWithOptions(mockGetPowerStatus,
		activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
	env.RegisterActivityWithOptions(mockVerifyReachability,
		activity.RegisterOptions{Name: "VerifyReachability"})
	registerTaskUpdateActivities(env)

	// Mock activity responses
	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)
	env.OnActivity(mockGetPowerStatus, mock.Anything,
		mock.Anything).Return(map[string]operations.PowerStatus{
		"ext-powershelf-1": operations.PowerStatusOn,
	}, nil)
	env.OnActivity(mockVerifyReachability, mock.Anything, mock.Anything,
		mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Rule with composite verification (power status + reachability)
	ruleDef := &operationrules.RuleDefinition{
		Version: "v1",
		Steps: []operationrules.SequenceStep{
			{
				ComponentType: devicetypes.ComponentTypePowerShelf,
				Stage:         1,
				MaxParallel:   0,
				Timeout:       10 * time.Minute,
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionPowerControl,
				},
				PostOperation: []operationrules.ActionConfig{
					{
						Name:         operationrules.ActionVerifyPowerStatus,
						Timeout:      15 * time.Second,
						PollInterval: 5 * time.Second,
						Parameters: map[string]any{
							operationrules.ParamExpectedStatus: "on",
						},
					},
					{
						Name:         operationrules.ActionVerifyReachability,
						Timeout:      3 * time.Minute,
						PollInterval: 10 * time.Second,
						Parameters: map[string]any{
							operationrules.ParamComponentTypes: []string{
								"compute",
								"nvswitch",
							},
						},
					},
				},
			},
		},
	}

	// Create test components
	components := []*component.Component{
		newTestComponent(uuid.New(), "powershelf-1", "ext-powershelf-1",
			devicetypes.ComponentTypePowerShelf),
	}

	info := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}

	reqInfo := taskdef.ExecutionInfo{
		TaskID:         uuid.New(),
		Components:     toWorkflowComponents(components),
		RuleDefinition: ruleDef,
	}

	// Register child workflow
	env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})

	// Execute workflow
	expectTaskUpdateActivities(env)
	env.ExecuteWorkflow(powerControl, reqInfo, info)

	// Verify workflow completed successfully
	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}
