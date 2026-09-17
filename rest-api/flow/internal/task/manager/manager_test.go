// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestCreateAndExecuteTaskReturnsExistingIdempotentTaskBeforeRackConflict(t *testing.T) {
	ctx := context.Background()
	rackID := uuid.New()
	componentID := uuid.New()
	taskID := uuid.New()
	idempotencyKey := "operation-run-target:" + uuid.NewString()
	op := testPowerControlOperation(t)
	targetRack := newTestRack(rackID, "rack-1")
	targetRack.AddComponent(newTestComponent(
		componentID,
		rackID,
		devicetypes.ComponentTypeCompute,
		"compute-1",
	))
	existingTask := &taskdef.Task{
		ID:             taskID,
		Operation:      op,
		RackID:         rackID,
		Status:         taskcommon.TaskStatusPending,
		ExecutionID:    `{"workflow_id":"workflow","run_id":"run"}`,
		IdempotencyKey: idempotencyKey,
		Attributes: taskcommon.TaskAttributes{
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute: {componentID},
			},
		},
	}
	store := &managerTaskStore{
		activeTasksByRack: map[uuid.UUID][]*taskdef.Task{
			rackID: {
				{
					ID:        uuid.New(),
					Operation: op,
					RackID:    rackID,
					Status:    taskcommon.TaskStatusRunning,
					Attributes: taskcommon.TaskAttributes{
						ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
							devicetypes.ComponentTypeCompute: {componentID},
						},
					},
				},
			},
		},
		taskByIdempotencyKey: map[string]*taskdef.Task{
			idempotencyKey: existingTask,
		},
	}
	manager := &ManagerImpl{
		taskStore:           store,
		conflictResolver:    conflict.NewResolver(store),
		maxWaitingPerRack:   defaultMaxWaitingPerRack,
		defaultQueueTimeout: defaultQueueTimeout,
	}

	gotTaskID, err := manager.createAndExecuteTask(ctx, &operation.Request{
		Operation:        op,
		Description:      "retry operation-run target",
		ConflictStrategy: operation.ConflictStrategyReject,
		RequiredRackID:   rackID,
		IdempotencyKey:   idempotencyKey,
	}, targetRack)

	require.NoError(t, err)
	require.Equal(t, taskID, gotTaskID)
	require.Zero(t, store.listActiveCalls)
	require.Zero(t, store.createTaskCalls)
}

func TestCreateAndExecuteTaskSchedulesExistingIdempotentTaskWithoutExecutionID(t *testing.T) {
	ctx := context.Background()
	rackID := uuid.New()
	componentID := uuid.New()
	taskID := uuid.New()
	idempotencyKey := "operation-run-target:" + uuid.NewString()
	op := testPowerControlOperation(t)
	targetRack := newTestRack(rackID, "rack-1")
	targetRack.AddComponent(newTestComponent(
		componentID,
		rackID,
		devicetypes.ComponentTypeCompute,
		"compute-1",
	))
	existingTask := &taskdef.Task{
		ID:             taskID,
		Operation:      op,
		RackID:         rackID,
		Status:         taskcommon.TaskStatusPending,
		IdempotencyKey: idempotencyKey,
		Attributes: taskcommon.TaskAttributes{
			ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute: {componentID},
			},
		},
	}
	store := &managerTaskStore{
		taskByIdempotencyKey: map[string]*taskdef.Task{
			idempotencyKey: existingTask,
		},
	}
	executor := &managerExecutor{executionID: `{"workflow_id":"workflow","run_id":"run"}`}
	manager := &ManagerImpl{
		taskStore:           store,
		executor:            executor,
		maxWaitingPerRack:   defaultMaxWaitingPerRack,
		defaultQueueTimeout: defaultQueueTimeout,
	}

	gotTaskID, err := manager.createAndExecuteTask(ctx, &operation.Request{
		Operation:        op,
		Description:      "retry operation-run target",
		ConflictStrategy: operation.ConflictStrategyReject,
		RequiredRackID:   rackID,
		IdempotencyKey:   idempotencyKey,
	}, targetRack)

	require.NoError(t, err)
	require.Equal(t, taskID, gotTaskID)
	require.Equal(t, 1, executor.executeCalls)
	require.Equal(t, taskID, executor.lastRequest.Info.TaskID)
	require.Equal(t, 1, store.updateScheduledCalls)
	require.Equal(t, executor.executionID, store.updatedScheduledTask.ExecutionID)
	require.Zero(t, store.listActiveCalls)
	require.Zero(t, store.createTaskCalls)
}

func TestManagerImpl_CancelTask(t *testing.T) {
	taskID := uuid.New()
	store := &managerTaskStore{tasksByID: map[uuid.UUID]*taskdef.Task{
		taskID: {
			ID:     taskID,
			Status: taskcommon.TaskStatusFailed,
		},
	}}
	executor := &managerExecutor{}
	manager := &ManagerImpl{taskStore: store, executor: executor}

	err := manager.CancelTask(context.Background(), taskID)

	require.ErrorIs(t, err, ErrTaskNotCancellable)
	require.ErrorContains(t, err, "status failed")
	require.Zero(t, executor.terminateCalls)
	require.Empty(t, store.statusUpdates)
}

func testPowerControlOperation(t *testing.T) operation.Wrapper {
	t.Helper()

	info, err := (&operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	}).Marshal()
	require.NoError(t, err)

	return operation.Wrapper{
		Type: taskcommon.TaskTypePowerControl,
		Code: taskcommon.OpCodePowerControlPowerOn,
		Info: info,
	}
}

type managerTaskStore struct {
	activeTasksByRack    map[uuid.UUID][]*taskdef.Task
	taskByIdempotencyKey map[string]*taskdef.Task
	tasksByID            map[uuid.UUID]*taskdef.Task
	listActiveCalls      int
	createTaskCalls      int
	lockKeyCalls         int
	lockRackCalls        int
	updateScheduledCalls int
	updatedScheduledTask *taskdef.Task
	statusUpdates        []*taskdef.TaskStatusUpdate
}

func (s *managerTaskStore) RunInTransaction(
	ctx context.Context,
	fn func(context.Context) error,
) error {
	return fn(ctx)
}

func (s *managerTaskStore) CreateTask(_ context.Context, _ *taskdef.Task) error {
	s.createTaskCalls++
	return fmt.Errorf("CreateTask should not be called")
}

func (s *managerTaskStore) LockRack(_ context.Context, _ uuid.UUID) error {
	s.lockRackCalls++
	return nil
}

func (s *managerTaskStore) LockIdempotencyKey(_ context.Context, _ string) error {
	s.lockKeyCalls++
	return nil
}

func (s *managerTaskStore) GetTaskByIdempotencyKey(
	_ context.Context,
	key string,
) (*taskdef.Task, error) {
	return s.taskByIdempotencyKey[key], nil
}

func (s *managerTaskStore) GetTask(_ context.Context, id uuid.UUID) (*taskdef.Task, error) {
	return s.tasksByID[id], nil
}

func (s *managerTaskStore) GetTasks(_ context.Context, _ []uuid.UUID) ([]*taskdef.Task, error) {
	panic("managerTaskStore.GetTasks: not implemented")
}

func (s *managerTaskStore) ListTasks(
	_ context.Context,
	_ *taskcommon.TaskListOptions,
	_ *dbquery.Pagination,
) ([]*taskdef.Task, int32, error) {
	panic("managerTaskStore.ListTasks: not implemented")
}

func (s *managerTaskStore) ListNonTerminalTasksForRacks(
	_ context.Context,
	_ []uuid.UUID,
) ([]*taskdef.Task, error) {
	panic("managerTaskStore.ListNonTerminalTasksForRacks: not implemented")
}

func (s *managerTaskStore) UpdateScheduledTask(_ context.Context, task *taskdef.Task) error {
	s.updateScheduledCalls++
	s.updatedScheduledTask = task
	return nil
}

func (s *managerTaskStore) UpdateTaskStatus(
	_ context.Context,
	update *taskdef.TaskStatusUpdate,
) error {
	s.statusUpdates = append(s.statusUpdates, update)
	return nil
}

func (s *managerTaskStore) UpdateTaskReport(
	_ context.Context,
	_ *taskdef.TaskReportUpdate,
) error {
	panic("managerTaskStore.UpdateTaskReport: not implemented")
}

func (s *managerTaskStore) ListActiveTasksForRack(
	_ context.Context,
	rackID uuid.UUID,
) ([]*taskdef.Task, error) {
	s.listActiveCalls++
	return s.activeTasksByRack[rackID], nil
}

func (s *managerTaskStore) ListWaitingTasksForRack(
	_ context.Context,
	_ uuid.UUID,
) ([]*taskdef.Task, error) {
	panic("managerTaskStore.ListWaitingTasksForRack: not implemented")
}

func (s *managerTaskStore) CountWaitingTasksForRack(_ context.Context, _ uuid.UUID) (int, error) {
	panic("managerTaskStore.CountWaitingTasksForRack: not implemented")
}

func (s *managerTaskStore) ListRacksWithWaitingTasks(_ context.Context) ([]uuid.UUID, error) {
	panic("managerTaskStore.ListRacksWithWaitingTasks: not implemented")
}

func (s *managerTaskStore) CreateRule(
	_ context.Context,
	_ *operationrules.OperationRule,
) error {
	panic("managerTaskStore.CreateRule: not implemented")
}

func (s *managerTaskStore) UpdateRule(
	_ context.Context,
	_ uuid.UUID,
	_ map[string]interface{},
) error {
	panic("managerTaskStore.UpdateRule: not implemented")
}

func (s *managerTaskStore) DeleteRule(_ context.Context, _ uuid.UUID) error {
	panic("managerTaskStore.DeleteRule: not implemented")
}

func (s *managerTaskStore) SetRuleAsDefault(_ context.Context, _ uuid.UUID) error {
	panic("managerTaskStore.SetRuleAsDefault: not implemented")
}

func (s *managerTaskStore) GetRule(
	_ context.Context,
	_ uuid.UUID,
) (*operationrules.OperationRule, error) {
	panic("managerTaskStore.GetRule: not implemented")
}

func (s *managerTaskStore) GetRuleByName(
	_ context.Context,
	_ string,
) (*operationrules.OperationRule, error) {
	panic("managerTaskStore.GetRuleByName: not implemented")
}

func (s *managerTaskStore) GetDefaultRule(
	_ context.Context,
	_ taskcommon.TaskType,
	_ string,
) (*operationrules.OperationRule, error) {
	panic("managerTaskStore.GetDefaultRule: not implemented")
}

func (s *managerTaskStore) GetRuleByOperationAndRack(
	_ context.Context,
	_ taskcommon.TaskType,
	_ string,
	_ *uuid.UUID,
) (*operationrules.OperationRule, error) {
	panic("managerTaskStore.GetRuleByOperationAndRack: not implemented")
}

func (s *managerTaskStore) ListRules(
	_ context.Context,
	_ *taskcommon.OperationRuleListOptions,
	_ *dbquery.Pagination,
) ([]*operationrules.OperationRule, int32, error) {
	panic("managerTaskStore.ListRules: not implemented")
}

func (s *managerTaskStore) AssociateRuleWithRack(
	_ context.Context,
	_ uuid.UUID,
	_ uuid.UUID,
) error {
	panic("managerTaskStore.AssociateRuleWithRack: not implemented")
}

func (s *managerTaskStore) DisassociateRuleFromRack(
	_ context.Context,
	_ uuid.UUID,
	_ taskcommon.TaskType,
	_ string,
) error {
	panic("managerTaskStore.DisassociateRuleFromRack: not implemented")
}

func (s *managerTaskStore) GetRackRuleAssociation(
	_ context.Context,
	_ uuid.UUID,
	_ taskcommon.TaskType,
	_ string,
) (*uuid.UUID, error) {
	panic("managerTaskStore.GetRackRuleAssociation: not implemented")
}

func (s *managerTaskStore) ListRackRuleAssociations(
	_ context.Context,
	_ uuid.UUID,
) ([]*operationrules.RackRuleAssociation, error) {
	panic("managerTaskStore.ListRackRuleAssociations: not implemented")
}

var _ interface {
	RunInTransaction(context.Context, func(context.Context) error) error
} = (*managerTaskStore)(nil)

type managerExecutor struct {
	executionID    string
	executeCalls   int
	lastRequest    *taskdef.ExecutionRequest
	terminateCalls int
}

func (e *managerExecutor) Start(context.Context) error {
	return nil
}

func (e *managerExecutor) Stop(context.Context) error {
	return nil
}

func (e *managerExecutor) Type() taskcommon.ExecutorType {
	return taskcommon.ExecutorTypeTemporal
}

func (e *managerExecutor) Execute(
	_ context.Context,
	req *taskdef.ExecutionRequest,
) (*taskdef.ExecutionResponse, error) {
	e.executeCalls++
	e.lastRequest = req
	return &taskdef.ExecutionResponse{ExecutionID: e.executionID}, nil
}

func (e *managerExecutor) CheckStatus(
	context.Context,
	string,
) (taskcommon.TaskStatus, error) {
	panic("managerExecutor.CheckStatus: not implemented")
}

func (e *managerExecutor) TerminateTask(context.Context, string, string) error {
	e.terminateCalls++
	return nil
}
