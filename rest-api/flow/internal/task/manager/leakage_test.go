// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"os"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type emergencyStore struct {
	taskstore.Store
	createErr error
}

func (s *emergencyStore) CreateTask(ctx context.Context, task *taskdef.Task) error {
	if isDirectForcePowerOff(task.Operation) && s.createErr != nil {
		return s.createErr
	}
	return s.Store.CreateTask(ctx, task)
}

type emergencyExecutor struct {
	managerExecutor
	executeErr error
}

func (e *emergencyExecutor) Execute(ctx context.Context, req *taskdef.ExecutionRequest) (*taskdef.ExecutionResponse, error) {
	if e.executeErr != nil {
		return nil, e.executeErr
	}
	return e.managerExecutor.Execute(ctx, req)
}

func TestManagerImpl_SubmitLeakageShutdown(t *testing.T) {
	t.Run("admission", testLeakageShutdownAdmission)
	t.Run("unsafe operations", testLeakageShutdownRejectsUnsafeOperations)
}

func testLeakageShutdownAdmission(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("PostgreSQL environment is required")
	}
	failure := errors.New("injected failure")
	for _, tc := range []struct {
		name                  string
		createErr, executeErr error
		waitingReservation    bool
	}{
		{name: "preempts a full queue and preserves idempotency"},
		{name: "upgrades a predecessor queued leakage task", waitingReservation: true},
		{name: "admission rollback preserves old ownership", createErr: failure},
		{name: "dispatch failure retains the committed fence", executeErr: failure},
	} {
		t.Run(tc.name, func(t *testing.T) {
			config, err := cdb.ConfigFromEnv()
			require.NoError(t, err)
			session, err := utils.UnitTestDB(t.Context(), t, config)
			require.NoError(t, err)
			t.Cleanup(session.Close)
			persisted := taskstore.NewPostgres(session)
			store := &emergencyStore{Store: persisted, createErr: tc.createErr}
			rackID, componentID, otherID := uuid.New(), uuid.New(), uuid.New()
			target := newTestRack(rackID, "rack-1")
			target.AddComponent(newTestComponent(componentID, rackID, devicetypes.ComponentTypeCompute, "machine-1"))
			executor := &emergencyExecutor{managerExecutor: managerExecutor{executionID: `{"workflowID":"emergency","runID":"run"}`, terminateErr: errors.New("termination unavailable")}, executeErr: tc.executeErr}
			manager := &ManagerImpl{taskStore: store, executor: executor, maxWaitingPerRack: 1, inventoryStore: &submitTaskInventory{rack: target}}
			op := testPowerControlOperation(t)
			var old []*taskdef.Task
			for _, status := range []taskcommon.TaskStatus{taskcommon.TaskStatusRunning, taskcommon.TaskStatusWaiting, taskcommon.TaskStatusWaiting} {
				task := &taskdef.Task{ID: uuid.New(), RackID: rackID, Operation: op, ExecutorType: taskcommon.ExecutorTypeUnknown, Status: status, Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {componentID}}}}
				if status == taskcommon.TaskStatusRunning {
					task.Operation = testFirmwareControlOperation(t)
					task.ExecutionID = "old-execution"
					task.ExecutorType = taskcommon.ExecutorTypeTemporal
				}
				require.NoError(t, persisted.CreateTask(t.Context(), task))
				old = append(old, task)
			}
			unrelated := &taskdef.Task{ID: uuid.New(), RackID: rackID, Operation: op, ExecutorType: taskcommon.ExecutorTypeTemporal, Status: taskcommon.TaskStatusRunning, Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {otherID}}}}
			require.NoError(t, persisted.CreateTask(t.Context(), unrelated))
			info := operations.PowerControlTaskInfo{Operation: operations.PowerOperationForcePowerOff, OverrideReadinessCheck: true}
			raw, err := info.Marshal()
			require.NoError(t, err)
			req := &operation.Request{Operation: operation.Wrapper{Type: info.Type(), Code: info.CodeString(), Info: raw}, IdempotencyKey: "leak-emergency", RequiredRackID: rackID, ConflictStrategy: operation.ConflictStrategyQueue, TargetSpec: operation.TargetSpec{Racks: []operation.RackTarget{{Identifier: identifier.Identifier{ID: rackID}}}}}
			var predecessorID uuid.UUID
			if tc.waitingReservation {
				predecessor := newTaskForRack(req, target)
				predecessor.Status = taskcommon.TaskStatusWaiting
				predecessorID = predecessor.ID
				require.NoError(t, persisted.CreateTask(t.Context(), &predecessor))
			}
			ids, err := manager.SubmitLeakageShutdown(t.Context(), req)
			if tc.createErr != nil || tc.executeErr != nil {
				require.ErrorIs(t, err, failure)
			} else {
				require.NoError(t, err)
				again, err := manager.SubmitLeakageShutdown(t.Context(), req)
				require.NoError(t, err)
				require.Len(t, ids, 1)
				if tc.waitingReservation {
					require.Equal(t, predecessorID, ids[0])
				}
				require.Equal(t, ids, again)
				require.Equal(t, 1, executor.executeCalls)
				require.NotEmpty(t, executor.lastRequest.Info.RuleDefinition.Steps)
			}
			for _, original := range old {
				got, err := persisted.GetTask(t.Context(), original.ID)
				require.NoError(t, err)
				if tc.createErr != nil {
					require.Equal(t, original.Status, got.Status)
					continue
				}
				require.Equal(t, taskcommon.TaskStatusTerminated, got.Status)
				require.NotNil(t, got.FinishedAt)
				require.Nil(t, got.QueueExpiresAt)
				require.Contains(t, got.Message, "resubmit manually")
				// An activity that was already running must not resurrect or overwrite
				// the terminal reason after its parent workflow was terminated.
				finishedAt, message := *got.FinishedAt, got.Message
				for _, late := range []taskcommon.TaskStatus{taskcommon.TaskStatusRunning, taskcommon.TaskStatusCompleted, taskcommon.TaskStatusFailed} {
					require.NoError(t, persisted.UpdateTaskStatus(t.Context(), &taskdef.TaskStatusUpdate{ID: got.ID, Status: late, Message: "late worker"}))
				}
				got, err = persisted.GetTask(t.Context(), original.ID)
				require.NoError(t, err)
				require.Equal(t, taskcommon.TaskStatusTerminated, got.Status)
				require.Equal(t, message, got.Message)
				require.True(t, finishedAt.Equal(*got.FinishedAt))
				fence := &notifyingTaskStore{Store: persisted}
				require.ErrorContains(t, fence.CheckTaskDispatch(t.Context(), original.ID), "no longer owns")
			}
			got, err := persisted.GetTask(t.Context(), unrelated.ID)
			require.NoError(t, err)
			require.Equal(t, taskcommon.TaskStatusRunning, got.Status)
			if tc.createErr == nil {
				require.Equal(t, 1, executor.terminateCalls)
			} else {
				require.Zero(t, executor.terminateCalls)
			}
		})
	}
}

func testLeakageShutdownRejectsUnsafeOperations(t *testing.T) {
	for _, tc := range []struct {
		name     string
		op       operations.PowerOperation
		override bool
	}{
		{name: "power on cannot preempt", op: operations.PowerOperationPowerOn, override: true},
		{name: "force off without readiness override cannot preempt", op: operations.PowerOperationForcePowerOff},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info := operations.PowerControlTaskInfo{Operation: tc.op, OverrideReadinessCheck: tc.override}
			raw, err := info.Marshal()
			require.NoError(t, err)
			manager := &ManagerImpl{}
			ids, err := manager.SubmitLeakageShutdown(t.Context(), &operation.Request{Operation: operation.Wrapper{Type: info.Type(), Code: info.CodeString(), Info: raw}})
			require.ErrorContains(t, err, "requires force power off")
			require.Nil(t, ids)
		})
	}
}

func testSubmitTaskDoesNotPreemptForcedShutdown(t *testing.T) {
	rackID, componentID := uuid.New(), uuid.New()
	target := newTestRack(rackID, "rack")
	target.AddComponent(newTestComponent(componentID, rackID, devicetypes.ComponentTypeCompute, "machine-1"))
	active := &taskdef.Task{ID: uuid.New(), RackID: rackID, Operation: testFirmwareControlOperation(t), Status: taskcommon.TaskStatusRunning, Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {componentID}}}}
	store := &managerTaskStore{activeTasksByRack: map[uuid.UUID][]*taskdef.Task{rackID: {active}}}
	manager := &ManagerImpl{taskStore: store, inventoryStore: &submitTaskInventory{rack: target}, executor: &managerExecutor{}, conflictResolver: conflict.NewResolver(store)}
	info := operations.PowerControlTaskInfo{Operation: operations.PowerOperationForcePowerOff, OverrideReadinessCheck: true}
	raw, err := info.Marshal()
	require.NoError(t, err)
	req := &operation.Request{Operation: operation.Wrapper{Type: info.Type(), Code: info.CodeString(), Info: raw}, ConflictStrategy: operation.ConflictStrategyReject, RequiredRackID: rackID, TargetSpec: operation.TargetSpec{Racks: []operation.RackTarget{{Identifier: identifier.Identifier{ID: rackID}}}}}
	_, err = manager.SubmitTask(t.Context(), req)
	require.ErrorIs(t, err, ErrRackConflict)
	require.Empty(t, store.statusUpdates)
	require.Zero(t, store.createTaskCalls)
}
