// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

func mockInjectExpectation(
	ctx context.Context,
	target common.Target,
	info operations.InjectExpectationTaskInfo,
) error {
	return nil
}

func TestInjectExpectationWorkflow(t *testing.T) {
	computeID1 := uuid.New()
	computeID2 := uuid.New()
	powershelfID := uuid.New()
	nvswitchID := uuid.New()

	fullComponents := []*component.Component{
		newTestComponent(powershelfID, "powershelf-1", "ext-powershelf-1", devicetypes.ComponentTypePowerShelf),
		newTestComponent(nvswitchID, "nvswitch-1", "ext-nvswitch-1", devicetypes.ComponentTypeNVSwitch),
		newTestComponent(computeID1, "compute-1", "ext-compute-1", devicetypes.ComponentTypeCompute),
		newTestComponent(computeID2, "compute-2", "ext-compute-2", devicetypes.ComponentTypeCompute),
	}

	computeOnly := []*component.Component{
		newTestComponent(computeID1, "compute-1", "ext-compute-1", devicetypes.ComponentTypeCompute),
		newTestComponent(computeID2, "compute-2", "ext-compute-2", devicetypes.ComponentTypeCompute),
	}

	powershelfOnly := []*component.Component{
		newTestComponent(powershelfID, "powershelf-1", "ext-powershelf-1", devicetypes.ComponentTypePowerShelf),
	}

	testCases := map[string]struct {
		components    []*component.Component
		activityError error
		expectError   bool
		errorContains string
	}{
		"full components success": {
			components:  fullComponents,
			expectError: false,
		},
		"compute only success": {
			components:  computeOnly,
			expectError: false,
		},
		"powershelf only success": {
			components:  powershelfOnly,
			expectError: false,
		},
		"activity failure returns error": {
			components:    computeOnly,
			activityError: errors.New("component manager service unavailable"),
			expectError:   true,
			errorContains: "component manager service unavailable",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			testSuite := &testsuite.WorkflowTestSuite{}
			env := testSuite.NewTestWorkflowEnvironment()

			env.RegisterActivityWithOptions(mockInjectExpectation, activity.RegisterOptions{
				Name: activitypkg.NameInjectExpectation,
			})
			registerTaskUpdateActivities(env)

			env.OnActivity(mockInjectExpectation, mock.Anything, mock.Anything, mock.Anything).Return(tc.activityError)

			info := &operations.InjectExpectationTaskInfo{}
			reqInfo := taskdef.ExecutionInfo{
				TaskID:     uuid.New(),
				Components: toWorkflowComponents(tc.components),
			}

			expectTaskUpdateActivities(env)
			env.ExecuteWorkflow(injectExpectation, reqInfo, info)

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
