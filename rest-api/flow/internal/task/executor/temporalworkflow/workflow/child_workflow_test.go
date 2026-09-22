// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Mock activities for testing
func mockVerifyPowerStatus(
	ctx context.Context,
	target common.Target,
	expectedStatus operations.PowerStatus,
	timeout time.Duration,
	pollInterval time.Duration,
) error {
	return nil
}

func mockVerifyReachability(
	ctx context.Context,
	allTargets map[devicetypes.ComponentType]common.Target,
	componentTypes []string,
	timeout time.Duration,
	pollInterval time.Duration,
) error {
	return nil
}

// TestGenericComponentStepWorkflow_ActionBased tests the new action-based
// execution with pre/main/post operations
func TestGenericComponentStepWorkflow_ActionBased(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	// Register activities with correct names
	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})
	env.RegisterActivityWithOptions(mockGetPowerStatus,
		activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
	env.RegisterActivityWithOptions(mockVerifyPowerStatus,
		activity.RegisterOptions{Name: "VerifyPowerStatus"})
	env.RegisterActivityWithOptions(mockVerifyReachability,
		activity.RegisterOptions{Name: "VerifyReachability"})

	// Mock activity responses
	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)
	// GetPowerStatus returns map of component IDs to power status
	env.OnActivity(mockGetPowerStatus, mock.Anything,
		mock.Anything).Return(map[string]operations.PowerStatus{
		"test-powershelf-1": operations.PowerStatusOn,
	}, nil)
	env.OnActivity(mockVerifyPowerStatus, mock.Anything, mock.Anything,
		mock.Anything, mock.Anything, mock.Anything).Return(nil)
	env.OnActivity(mockVerifyReachability, mock.Anything, mock.Anything,
		mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Create test step with action-based configuration
	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypePowerShelf,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		RetryPolicy: &operationrules.RetryPolicy{
			MaxAttempts:        3,
			InitialInterval:    5 * time.Second,
			BackoffCoefficient: 2.0,
		},
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
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"test-powershelf-1"},
	}

	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypePowerShelf: target,
	}

	operationInfo := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}

	// Execute workflow
	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		operationInfo, allTargets)

	// Verify workflow completed successfully
	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_WithSleepAction tests Sleep action in
// post-operations
func TestGenericComponentStepWorkflow_WithSleepAction(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	// Register activities with correct names
	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})

	// Mock activity responses
	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)

	// Create test step with Sleep action
	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypePowerShelf,
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
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"test-powershelf-1"},
	}

	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypePowerShelf: target,
	}

	operationInfo := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}

	// Execute workflow
	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		operationInfo, allTargets)

	// Verify workflow completed successfully
	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_VerificationFailure tests workflow
// behavior when verification fails
func TestGenericComponentStepWorkflow_VerificationFailure(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	// Register activities with correct names
	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})
	env.RegisterActivity(mockVerifyPowerStatus)

	// Mock activity responses - verification fails
	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)
	env.OnActivity(mockVerifyPowerStatus, mock.Anything, mock.Anything,
		mock.Anything, mock.Anything,
		mock.Anything).Return(errors.New("verification timeout"))

	// Create test step with verification
	step := operationrules.SequenceStep{
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
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"test-powershelf-1"},
	}

	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypePowerShelf: target,
	}

	operationInfo := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}

	// Execute workflow
	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		operationInfo, allTargets)

	// Verify workflow completed with error
	assert.True(t, env.IsWorkflowCompleted())
	assert.Error(t, env.GetWorkflowError())
	assert.Contains(t, env.GetWorkflowError().Error(),
		"post-operation failed")
}

// TestGenericComponentStepWorkflow_PreOperation tests pre-operation
// execution
func TestGenericComponentStepWorkflow_PreOperation(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	// Register activities with correct names
	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})

	// Mock activity responses
	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)

	// Create test step with pre-operation Sleep
	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypePowerShelf,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		PreOperation: []operationrules.ActionConfig{
			{
				Name: operationrules.ActionSleep,
				Parameters: map[string]any{
					operationrules.ParamDuration: 5 * time.Second,
				},
			},
		},
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionPowerControl,
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"test-powershelf-1"},
	}

	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypePowerShelf: target,
	}

	operationInfo := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOff,
	}

	// Execute workflow
	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		operationInfo, allTargets)

	// Verify workflow completed successfully
	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_VerifyReachabilityRequireAll tests that
// VerifyReachability with require_all=true succeeds when GetPowerStatus
// returns all requested individual components.
func TestGenericComponentStepWorkflow_VerifyReachabilityRequireAll(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	env.RegisterActivityWithOptions(mockGetPowerStatus,
		activity.RegisterOptions{Name: activitypkg.NameGetPowerStatus})
	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})

	env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).Return(
		map[string]operations.PowerStatus{
			"ps-1": operations.PowerStatusOff,
		}, nil)
	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypePowerShelf,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		PreOperation: []operationrules.ActionConfig{
			{
				Name:         operationrules.ActionVerifyReachability,
				Timeout:      5 * time.Second,
				PollInterval: 1 * time.Second,
				Parameters: map[string]any{
					operationrules.ParamComponentTypes: []string{"powershelf"},
					operationrules.ParamRequireAll:     true,
				},
			},
		},
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionPowerControl,
			Parameters: map[string]any{
				operationrules.ParamOperation: "power_on",
			},
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1"},
	}
	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypePowerShelf: target,
	}

	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		&operations.BringUpTaskInfo{}, allTargets)

	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_BringUpAndWait tests BringUp as
// MainOperation and WaitBringUp as PostOperation.
func TestGenericComponentStepWorkflow_BringUpAndWait(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	mockBringUpControl := func(_ context.Context, _ common.Target, _ operations.BringUpTaskInfo) error {
		return nil
	}
	mockGetBringUpStatus := func(ctx context.Context, target common.Target) (*activitypkg.GetBringUpStatusResult, error) {
		return nil, nil
	}

	env.RegisterActivityWithOptions(mockBringUpControl,
		activity.RegisterOptions{Name: activitypkg.NameBringUpControl})
	env.RegisterActivityWithOptions(mockGetBringUpStatus,
		activity.RegisterOptions{Name: activitypkg.NameGetBringUpStatus})

	env.OnActivity(mockBringUpControl, mock.Anything, mock.Anything, mock.Anything).Return(nil)
	env.OnActivity(mockGetBringUpStatus, mock.Anything, mock.Anything).Return(
		&activitypkg.GetBringUpStatusResult{
			States: map[string]operations.MachineBringUpState{
				"compute-1": operations.MachineBringUpStateMachineCreated,
			},
		}, nil)

	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypeCompute,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionBringUpControl,
		},
		PostOperation: []operationrules.ActionConfig{
			{
				Name:         operationrules.ActionWaitBringUp,
				Timeout:      5 * time.Second,
				PollInterval: 1 * time.Second,
			},
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"compute-1"},
	}
	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypeCompute: target,
	}

	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		&operations.BringUpTaskInfo{}, allTargets)

	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_FirmwareControlAction tests the
// FirmwareControl action executor with start + poll pattern.
func TestGenericComponentStepWorkflow_FirmwareControlAction(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	mockStart := func(ctx context.Context, target common.Target, info operations.FirmwareControlTaskInfo) error {
		return nil
	}
	mockStatus := func(ctx context.Context, target common.Target) (*activitypkg.GetFirmwareStatusResult, error) {
		return nil, nil
	}

	env.RegisterActivityWithOptions(mockStart,
		activity.RegisterOptions{Name: activitypkg.NameFirmwareControl})
	env.RegisterActivityWithOptions(mockStatus,
		activity.RegisterOptions{Name: activitypkg.NameGetFirmwareStatus})

	env.OnActivity(mockStart, mock.Anything, mock.Anything, mock.Anything).Return(nil)
	env.OnActivity(mockStatus, mock.Anything, mock.Anything).Return(
		&activitypkg.GetFirmwareStatusResult{
			Statuses: map[string]operations.FirmwareUpdateStatus{
				"compute-1": {
					ComponentID: "compute-1",
					State:       operations.FirmwareUpdateStateCompleted,
				},
			},
		}, nil)

	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypeCompute,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionFirmwareControl,
			Parameters: map[string]any{
				operationrules.ParamPollInterval: "1s",
				operationrules.ParamPollTimeout:  "10s",
			},
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"compute-1"},
	}
	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypeCompute: target,
	}

	info := &operations.FirmwareControlTaskInfo{
		Operation: operations.FirmwareOperationUpgrade,
		StartTime: time.Now().Unix(),
		EndTime:   time.Now().Add(2 * time.Hour).Unix(),
	}

	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		info, allTargets)

	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_PowerControlWithParamOperation tests that
// PowerControl action constructs PowerControlTaskInfo from ParamOperation
// when the workflow's operationInfo is a different type (cross-workflow use).
func TestGenericComponentStepWorkflow_PowerControlWithParamOperation(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	env.RegisterActivityWithOptions(mockPowerControl,
		activity.RegisterOptions{Name: activitypkg.NamePowerControl})

	env.OnActivity(mockPowerControl, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)

	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypeCompute,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionPowerControl,
			Parameters: map[string]any{
				operationrules.ParamOperation: "force_power_off",
			},
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"compute-1"},
	}
	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypeCompute: target,
	}

	// Pass FirmwareControlTaskInfo as operationInfo -- NOT PowerControlTaskInfo.
	// The action executor should use ParamOperation instead.
	firmwareInfo := &operations.FirmwareControlTaskInfo{
		Operation: operations.FirmwareOperationUpgrade,
		StartTime: time.Now().Unix(),
		EndTime:   time.Now().Add(2 * time.Hour).Unix(),
	}

	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		firmwareInfo, allTargets)

	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_InjectExpectationAction tests the
// InjectExpectation action executor used in ingestion and full bring-up rules.
func TestGenericComponentStepWorkflow_InjectExpectationAction(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	mockInjectExpectation := func(
		ctx context.Context,
		target common.Target,
		info operations.InjectExpectationTaskInfo,
	) error {
		return nil
	}

	env.RegisterActivityWithOptions(mockInjectExpectation,
		activity.RegisterOptions{Name: activitypkg.NameInjectExpectation})

	env.OnActivity(mockInjectExpectation, mock.Anything, mock.Anything,
		mock.Anything).Return(nil)

	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypePowerShelf,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionInjectExpectation,
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypePowerShelf,
		Identifiers: []string{"ps-1", "ps-2"},
	}
	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypePowerShelf: target,
	}

	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		&operations.BringUpTaskInfo{}, allTargets)

	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}

// TestGenericComponentStepWorkflow_InjectExpectationFailure verifies that
// InjectExpectation action propagates activity errors.
func TestGenericComponentStepWorkflow_InjectExpectationFailure(t *testing.T) {
	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	mockInjectExpectation := func(
		ctx context.Context,
		target common.Target,
		info operations.InjectExpectationTaskInfo,
	) error {
		return nil
	}

	env.RegisterActivityWithOptions(mockInjectExpectation,
		activity.RegisterOptions{Name: activitypkg.NameInjectExpectation})

	env.OnActivity(mockInjectExpectation, mock.Anything, mock.Anything,
		mock.Anything).Return(errors.New("component manager service unavailable"))

	step := operationrules.SequenceStep{
		ComponentType: devicetypes.ComponentTypeCompute,
		Stage:         1,
		MaxParallel:   0,
		Timeout:       10 * time.Minute,
		MainOperation: operationrules.ActionConfig{
			Name: operationrules.ActionInjectExpectation,
		},
	}

	target := common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"compute-1"},
	}
	allTargets := map[devicetypes.ComponentType]common.Target{
		devicetypes.ComponentTypeCompute: target,
	}

	env.ExecuteWorkflow(genericComponentStepWorkflow, step, target,
		&operations.BringUpTaskInfo{}, allTargets)

	assert.True(t, env.IsWorkflowCompleted())
	assert.Error(t, env.GetWorkflowError())
}
