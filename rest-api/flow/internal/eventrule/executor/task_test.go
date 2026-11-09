// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/leakage"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestTaskExecutorExecute(t *testing.T) {
	t.Run("idempotent target submission", testTaskExecutorSubmit)
	t.Run("failure classification", testTaskExecutorClassifiesFailures)
	t.Run("leakage routing", testTaskExecutorLeakageRouting)
}

func testTaskExecutorSubmit(t *testing.T) {
	rackID := uuid.New()
	componentID := uuid.New()
	request := submitTaskExecutionRequest(t, rackID, componentID)
	manager := &recordingTaskManager{taskIDs: []uuid.UUID{uuid.New()}}
	executor := &TaskExecutor{manager: manager}

	require.NoError(t, executor.Execute(context.Background(), request))
	require.Len(t, manager.requests, 1)

	submitted := manager.requests[0]

	require.Equal(t, rackID, submitted.RequiredRackID)
	require.Equal(t, taskIdempotencyKey(request.ExecutionID, rackID), submitted.IdempotencyKey)
	require.Equal(t, operation.TriggerTypeEventRuleExecution, submitted.TriggerType)
	require.Equal(t, request.ExecutionID, *submitted.TriggerID)
	require.Equal(t, operation.TargetSpec{
		Components: []operation.ComponentTarget{{UUID: componentID}},
	}, submitted.TargetSpec)

	// A retry resubmits the same idempotency key so the task manager can return
	// the existing task.
	require.NoError(t, executor.Execute(context.Background(), request))
	require.Len(t, manager.requests, 2)
	require.Equal(t, submitted.IdempotencyKey, manager.requests[1].IdempotencyKey)
}

func testTaskExecutorClassifiesFailures(t *testing.T) {
	request := submitTaskExecutionRequest(t, uuid.New(), uuid.New())
	tests := map[string]struct {
		manager        *recordingTaskManager
		mutate         func(*ExecutionRequest)
		wantErr        string
		classification error
	}{
		"submission failure is retryable": {
			manager:        &recordingTaskManager{err: errors.New("task service unavailable")},
			wantErr:        "task service unavailable",
			classification: ErrRetryable,
		},
		"invalid task count is terminal": {
			manager:        &recordingTaskManager{},
			wantErr:        "exactly one valid task id",
			classification: ErrTerminal,
		},
		"wrong plan is terminal": {
			manager: &recordingTaskManager{},
			mutate: func(request *ExecutionRequest) {
				request.Plan = &eventrule.NoopPlan{}
			},
			wantErr:        "received plan",
			classification: ErrTerminal,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			input := request
			input.Plan = eventrule.CloneExecutionPlan(request.Plan)

			if test.mutate != nil {
				test.mutate(&input)
			}

			executor := &TaskExecutor{manager: test.manager}

			err := executor.Execute(context.Background(), input)

			require.ErrorContains(t, err, test.wantErr)
			require.ErrorIs(t, err, test.classification)
		})
	}
}

func submitTaskExecutionRequest(
	t *testing.T,
	rackID uuid.UUID,
	componentID uuid.UUID,
) ExecutionRequest {
	t.Helper()

	operationInfo := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationForcePowerOff,
	}

	info, err := operationInfo.Marshal()
	require.NoError(t, err)

	execution, err := eventrule.NewExecution(
		uuid.New(),
		"power_off",
		&eventrule.SubmitTaskPlan{
			Operation: operation.Wrapper{
				Type: taskcommon.TaskTypePowerControl,
				Code: operationInfo.CodeString(),
				Info: info,
			},
			Description:      "Power off affected component",
			ConflictStrategy: operation.ConflictStrategyQueue,
			Targets: []operation.RackExecutionTarget{{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: []uuid.UUID{componentID},
				},
			}},
		},
		testTime,
	)
	require.NoError(t, err)

	return ExecutionRequest{
		ExecutionID: execution.ID,
		Plan:        execution.Plan,
	}
}

var testTime = mustTestTime()

func mustTestTime() time.Time {
	return time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
}

type recordingTaskManager struct {
	requests        []*operation.Request
	leakageRequests []*operation.Request
	taskIDs         []uuid.UUID
	err             error
}

func (m *recordingTaskManager) SubmitTask(
	_ context.Context,
	request *operation.Request,
) ([]uuid.UUID, error) {
	m.requests = append(m.requests, request)

	return m.taskIDs, m.err
}

func (m *recordingTaskManager) SubmitLeakageShutdown(_ context.Context, req *operation.Request) ([]uuid.UUID, error) {
	m.leakageRequests = append(m.leakageRequests, req)
	return m.taskIDs, m.err
}

type eventTypeReaderFunc func(context.Context, uuid.UUID) (eventrule.Type, error)

func (f eventTypeReaderFunc) GetEventType(ctx context.Context, id uuid.UUID) (eventrule.Type, error) {
	return f(ctx, id)
}

func testTaskExecutorLeakageRouting(t *testing.T) {
	for _, tc := range []struct {
		name                                         string
		kind                                         eventrule.Type
		op                                           operations.PowerOperation
		override, preempt, readErr, missingSubmitter bool
	}{
		{name: "leakage force off uses dedicated admission", kind: leakage.TypeHardwareLeakDetected, op: operations.PowerOperationForcePowerOff, override: true, preempt: true},
		{name: "other event force off uses ordinary admission", kind: "test.other", op: operations.PowerOperationForcePowerOff, override: true},
		{name: "leakage power on keeps ordinary admission", kind: leakage.TypeHardwareLeakDetected, op: operations.PowerOperationPowerOn, override: true},
		{name: "predecessor policy without override retains its payload", kind: leakage.TypeHardwareLeakDetected, op: operations.PowerOperationForcePowerOff},
		{name: "provenance lookup failure submits nothing", op: operations.PowerOperationForcePowerOff, override: true, readErr: true},
		{name: "missing leakage capability never falls back", kind: leakage.TypeHardwareLeakDetected, op: operations.PowerOperationForcePowerOff, override: true, missingSubmitter: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			request := submitTaskExecutionRequest(t, uuid.New(), uuid.New())
			request.EventID = uuid.New()
			plan := request.Plan.(*eventrule.SubmitTaskPlan)
			info := operations.PowerControlTaskInfo{Operation: tc.op, OverrideReadinessCheck: tc.override}
			raw, err := info.Marshal()
			require.NoError(t, err)
			plan.Operation = operation.Wrapper{Type: info.Type(), Code: info.CodeString(), Info: raw}
			manager := &recordingTaskManager{taskIDs: []uuid.UUID{uuid.New()}}
			executor := &TaskExecutor{manager: manager, events: eventTypeReaderFunc(func(_ context.Context, id uuid.UUID) (eventrule.Type, error) {
				require.Equal(t, request.EventID, id)
				if tc.readErr {
					return "", errors.New("database unavailable")
				}
				return tc.kind, nil
			})}
			if tc.missingSubmitter {
				executor.manager = struct{ TaskManager }{manager}
			}
			err = executor.Execute(t.Context(), request)
			if tc.readErr || tc.missingSubmitter {
				require.Error(t, err)
				require.Empty(t, manager.requests)
				require.Empty(t, manager.leakageRequests)
				if tc.readErr {
					require.ErrorIs(t, err, ErrRetryable)
				} else {
					require.ErrorIs(t, err, ErrTerminal)
				}
			} else {
				require.NoError(t, err)
				if tc.preempt {
					require.Len(t, manager.leakageRequests, 1)
					require.Empty(t, manager.requests)
				} else {
					require.Len(t, manager.requests, 1)
					require.Empty(t, manager.leakageRequests)
				}
			}
		})
	}
}
