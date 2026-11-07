// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	sdkactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	temporalworkflow "go.temporal.io/sdk/workflow"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskactivity "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// --- WorkflowDescriptor.validate() ---

func TestWorkflowDescriptor_Validate(t *testing.T) {
	validFunc := func() {}
	validUnmarshal := func(_ json.RawMessage) (any, error) { return nil, nil }

	t.Run("valid internal workflow", func(t *testing.T) {
		d := WorkflowDescriptor{
			WorkflowName: "InternalWorkflow",
			WorkflowFunc: validFunc,
			// TaskType, Timeout, Unmarshal all zero/nil — allowed for internal workflows
		}
		assert.NoError(t, d.validate())
	})

	t.Run("valid task-dispatched workflow", func(t *testing.T) {
		d := WorkflowDescriptor{
			TaskType:     taskcommon.TaskTypePowerControl,
			WorkflowName: "PowerControl",
			WorkflowFunc: validFunc,
			Unmarshal:    validUnmarshal,
		}
		assert.NoError(t, d.validate())
	})

	t.Run("empty WorkflowName", func(t *testing.T) {
		d := WorkflowDescriptor{
			WorkflowFunc: validFunc,
		}
		assert.ErrorContains(t, d.validate(), "WorkflowName")
	})

	t.Run("nil WorkflowFunc", func(t *testing.T) {
		d := WorkflowDescriptor{
			WorkflowName: "MyWorkflow",
			WorkflowFunc: nil,
		}
		assert.ErrorContains(t, d.validate(), "WorkflowFunc")
	})

	t.Run("task-dispatched with nil Unmarshal", func(t *testing.T) {
		d := WorkflowDescriptor{
			TaskType:     taskcommon.TaskTypePowerControl,
			WorkflowName: "PowerControl",
			WorkflowFunc: validFunc,
			Unmarshal:    nil,
		}
		assert.ErrorContains(t, d.validate(), "Unmarshal")
	})

	t.Run("internal workflow with nil Unmarshal is allowed", func(t *testing.T) {
		d := WorkflowDescriptor{
			WorkflowName: "InternalWorkflow",
			WorkflowFunc: validFunc,
			Unmarshal:    nil,
		}
		assert.NoError(t, d.validate())
	})
}

// --- GetAllWorkflows ---

// TestGetAllWorkflows_DeterministicOrder verifies that GetAllWorkflows returns
// a stable, complete snapshot: same order on repeated calls and every registered
// workflow present exactly once with a non-nil function.
func TestGetAllWorkflows_DeterministicOrder(t *testing.T) {
	first := GetAllWorkflows()
	second := GetAllWorkflows()

	require.NotEmpty(t, first)
	require.Len(t, first, len(globalRegistry.workflows), "snapshot length must equal registry size")

	// Same order on every call.
	require.Len(t, second, len(first))
	for i := range first {
		assert.Equal(t, first[i].WorkflowName, second[i].WorkflowName,
			"position %d differs between calls", i)
	}

	// Each workflow name appears exactly once and has a non-nil function.
	seen := make(map[string]int, len(first))
	for _, wf := range first {
		seen[wf.WorkflowName]++
		assert.NotNil(t, wf.WorkflowFunc, "WorkflowFunc for %q must not be nil", wf.WorkflowName)
	}
	for name, count := range seen {
		assert.Equal(t, 1, count, "workflow %q appears %d times", name, count)
	}
}

// --- Per-descriptor registration and unmarshal ---

// mustGetDescriptor retrieves a WorkflowDescriptor from the registry and fails
// the test immediately if the descriptor is not registered.
func mustGetDescriptor(t *testing.T, taskType taskcommon.TaskType) WorkflowDescriptor {
	t.Helper()
	desc, ok := Get(taskType)
	require.True(t, ok, "workflow descriptor for %q not found in registry — check init() registration", taskType)
	require.NotNil(t, desc.Unmarshal, "Unmarshal function must not be nil for %q", taskType)
	return desc
}

// --- PowerControl ---

func TestWorkflowDescriptor_Registered_PowerControl(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypePowerControl)
	assert.Equal(t, "PowerControl", desc.WorkflowName)
	assert.NotNil(t, desc.WorkflowFunc)
	assert.Greater(t, desc.Timeout, time.Duration(0), "expected non-zero timeout")
}

func TestWorkflowDescriptor_Unmarshal_PowerControl(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypePowerControl)

	t.Run("valid operation", func(t *testing.T) {
		raw, _ := json.Marshal(map[string]any{"operation": operations.PowerOperationPowerOn})
		result, err := desc.Unmarshal(raw)
		require.NoError(t, err)
		info, ok := result.(*operations.PowerControlTaskInfo)
		require.True(t, ok, "expected *PowerControlTaskInfo, got %T", result)
		assert.Equal(t, operations.PowerOperationPowerOn, info.Operation)
	})

	t.Run("invalid JSON", func(t *testing.T) {
		_, err := desc.Unmarshal(json.RawMessage(`{not valid json`))
		assert.Error(t, err)
	})

	t.Run("unknown operation fails validation", func(t *testing.T) {
		raw, _ := json.Marshal(map[string]any{"operation": operations.PowerOperationUnknown})
		_, err := desc.Unmarshal(raw)
		assert.Error(t, err, "expected validation error for unknown power operation")
	})
}

// --- FirmwareControl ---

func TestWorkflowDescriptor_Registered_FirmwareControl(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypeFirmwareControl)
	assert.Equal(t, "FirmwareControl", desc.WorkflowName)
	assert.NotNil(t, desc.WorkflowFunc)
	assert.Greater(t, desc.Timeout, time.Duration(0), "expected non-zero timeout")
}

func TestWorkflowDescriptor_Unmarshal_FirmwareControl(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypeFirmwareControl)

	t.Run("valid operation", func(t *testing.T) {
		raw, _ := json.Marshal(map[string]any{"operation": operations.FirmwareOperationUpgrade})
		result, err := desc.Unmarshal(raw)
		require.NoError(t, err)
		info, ok := result.(*operations.FirmwareControlTaskInfo)
		require.True(t, ok, "expected *FirmwareControlTaskInfo, got %T", result)
		assert.Equal(t, operations.FirmwareOperationUpgrade, info.Operation)
	})

	t.Run("invalid JSON", func(t *testing.T) {
		_, err := desc.Unmarshal(json.RawMessage(`{not valid json`))
		assert.Error(t, err)
	})

	t.Run("unknown operation fails validation", func(t *testing.T) {
		raw, _ := json.Marshal(map[string]any{"operation": operations.FirmwareOperationUnknown})
		_, err := desc.Unmarshal(raw)
		assert.Error(t, err, "expected validation error for unknown firmware operation")
	})
}

// --- BringUp ---

func TestWorkflowDescriptor_Registered_BringUp(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypeBringUp)
	assert.Equal(t, "BringUp", desc.WorkflowName)
	assert.NotNil(t, desc.WorkflowFunc)
	assert.Greater(t, desc.Timeout, time.Duration(0), "expected non-zero timeout")
}

func TestWorkflowDescriptor_Unmarshal_BringUp(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypeBringUp)

	t.Run("valid empty payload", func(t *testing.T) {
		result, err := desc.Unmarshal(json.RawMessage(`{}`))
		require.NoError(t, err)
		_, ok := result.(*operations.BringUpTaskInfo)
		assert.True(t, ok, "expected *BringUpTaskInfo, got %T", result)
	})

	t.Run("valid with rule_id", func(t *testing.T) {
		raw, _ := json.Marshal(map[string]any{"rule_id": "test-rule"})
		result, err := desc.Unmarshal(raw)
		require.NoError(t, err)
		info, ok := result.(*operations.BringUpTaskInfo)
		require.True(t, ok, "expected *BringUpTaskInfo, got %T", result)
		assert.Equal(t, "test-rule", info.RuleID)
	})

	t.Run("invalid JSON", func(t *testing.T) {
		_, err := desc.Unmarshal(json.RawMessage(`{not valid json`))
		assert.Error(t, err)
	})
}

// --- InjectExpectation ---

func TestWorkflowDescriptor_Registered_InjectExpectation(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypeInjectExpectation)
	assert.Equal(t, "InjectExpectation", desc.WorkflowName)
	assert.NotNil(t, desc.WorkflowFunc)
	assert.Greater(t, desc.Timeout, time.Duration(0), "expected non-zero timeout")
}

func TestWorkflowDescriptor_Unmarshal_InjectExpectation(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypeInjectExpectation)

	t.Run("valid empty payload", func(t *testing.T) {
		result, err := desc.Unmarshal(json.RawMessage(`{}`))
		require.NoError(t, err)
		_, ok := result.(*operations.InjectExpectationTaskInfo)
		assert.True(t, ok, "expected *InjectExpectationTaskInfo, got %T", result)
	})

	t.Run("invalid JSON", func(t *testing.T) {
		_, err := desc.Unmarshal(json.RawMessage(`{not valid json`))
		assert.Error(t, err)
	})
}

// --- Registry/name dispatch integration ---

// TestRegistryNameDispatch verifies the full registration contract introduced
// by registerTaskWorkflow: Get() finds the descriptor, Unmarshal deserializes a
// real payload into the correct typed struct, and ExecuteWorkflow dispatches by
// WorkflowName (not by function reference). A broken name binding, a missing
// registry entry, or a mis-wired Unmarshal closure would all cause this test to
// fail while the per-workflow tests (which call the function directly) would not.
func TestRegistryNameDispatch(t *testing.T) {
	desc := mustGetDescriptor(t, taskcommon.TaskTypePowerControl)

	// Exercise Unmarshal: raw JSON → typed payload, same path as manager.Execute().
	raw, err := json.Marshal(map[string]any{"operation": operations.PowerOperationPowerOn})
	require.NoError(t, err)
	typedInfo, err := desc.Unmarshal(raw)
	require.NoError(t, err)

	testSuite := &testsuite.WorkflowTestSuite{}
	env := testSuite.NewTestWorkflowEnvironment()

	// Register the workflow by its descriptor name, mirroring Build()'s
	// RegisterWorkflowWithOptions call — not by function reference.
	env.RegisterWorkflowWithOptions(
		desc.WorkflowFunc,
		temporalworkflow.RegisterOptions{Name: desc.WorkflowName},
	)
	env.RegisterWorkflowWithOptions(genericComponentStepWorkflow, temporalworkflow.RegisterOptions{Name: nameGenericComponentStepWorkflow})

	registerTaskUpdateActivities(env)
	env.RegisterActivityWithOptions(mockPowerControl, sdkactivity.RegisterOptions{
		Name: taskactivity.NamePowerControl,
	})
	env.RegisterActivityWithOptions(mockGetPowerStatus, sdkactivity.RegisterOptions{
		Name: taskactivity.NameGetPowerStatus,
	})

	env.OnActivity(taskactivity.NamePowerControl, mock.Anything, mock.Anything, mock.Anything).Return(nil)
	expectTaskUpdateActivities(env)
	env.OnActivity(taskactivity.NameGetPowerStatus, mock.Anything, mock.Anything).Return(
		map[string]operations.PowerStatus{"ext-compute-1": operations.PowerStatusOn}, nil,
	)

	reqInfo := taskdef.ExecutionInfo{
		TaskID: uuid.New(),
		Components: []taskdef.WorkflowComponent{
			{Type: devicetypes.ComponentTypeCompute, ComponentID: "ext-compute-1"},
		},
		RuleDefinition: createDefaultPowerRuleDef(operations.PowerOperationPowerOn),
	}

	// Dispatch by registered name, not by function reference.
	env.ExecuteWorkflow(desc.WorkflowName, reqInfo, typedInfo)

	assert.True(t, env.IsWorkflowCompleted())
	assert.NoError(t, env.GetWorkflowError())
}
