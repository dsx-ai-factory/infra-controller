// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/store"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

type submitTaskInventory struct {
	inventorystore.Store
	rack         *rack.Rack
	getRackCalls int
}

func (s *submitTaskInventory) GetRackByIdentifier(
	_ context.Context,
	_ identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	s.getRackCalls++
	return s.rack, nil
}

func TestManagerImpl_SubmitTask(t *testing.T) {
	t.Run("rejects an unlinked operation target", func(t *testing.T) {
		rackID := uuid.New()
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinkedID := uuid.New()
		unlinked := newTestComponent(
			unlinkedID,
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)

		store := &managerTaskStore{}
		manager := &ManagerImpl{
			inventoryStore: &submitTaskInventory{rack: resolvedRack},
			taskStore:      store,
		}

		_, err := manager.SubmitTask(context.Background(), &operation.Request{
			Operation: testPowerControlOperation(t),
			TargetSpec: operation.TargetSpec{
				Racks: []operation.RackTarget{{
					Identifier: identifier.Identifier{ID: rackID},
				}},
			},
		})

		require.ErrorContains(t, err, "selected components not linked to actual inventory (1)")
		require.ErrorContains(t, err, unlinkedID.String())
		require.Zero(t, store.createTaskCalls)
	})

	t.Run("returns a scheduled idempotent task before inventory validation", func(t *testing.T) {
		rackID := uuid.New()
		taskID := uuid.New()
		idempotencyKey := "operation-run-target:" + uuid.NewString()
		store := &managerTaskStore{
			taskByIdempotencyKey: map[string]*taskdef.Task{
				idempotencyKey: {
					ID:             taskID,
					RackID:         rackID,
					ExecutionID:    `{"workflow_id":"workflow","run_id":"run"}`,
					IdempotencyKey: idempotencyKey,
				},
			},
		}
		inventory := &submitTaskInventory{}
		manager := &ManagerImpl{inventoryStore: inventory, taskStore: store}

		taskIDs, err := manager.SubmitTask(context.Background(), &operation.Request{
			Operation:      testPowerControlOperation(t),
			RequiredRackID: rackID,
			IdempotencyKey: idempotencyKey,
			TargetSpec: operation.TargetSpec{
				Racks: []operation.RackTarget{{
					Identifier: identifier.Identifier{ID: rackID},
				}},
			},
		})

		require.NoError(t, err)
		require.Equal(t, []uuid.UUID{taskID}, taskIDs)
		require.Zero(t, inventory.getRackCalls)
		require.Zero(t, store.createTaskCalls)
	})

	t.Run("returns a waiting idempotent task before inventory validation", func(t *testing.T) {
		rackID := uuid.New()
		taskID := uuid.New()
		idempotencyKey := "operation-run-target:" + uuid.NewString()
		deadline := time.Now().Add(time.Hour)
		store := &managerTaskStore{
			taskByIdempotencyKey: map[string]*taskdef.Task{
				idempotencyKey: {
					ID:             taskID,
					RackID:         rackID,
					Status:         taskcommon.TaskStatusWaiting,
					QueueExpiresAt: &deadline,
					IdempotencyKey: idempotencyKey,
				},
			},
		}
		inventory := &submitTaskInventory{}
		manager := &ManagerImpl{inventoryStore: inventory, taskStore: store}

		taskIDs, err := manager.SubmitTask(context.Background(), &operation.Request{
			Operation:      testPowerControlOperation(t),
			RequiredRackID: rackID,
			IdempotencyKey: idempotencyKey,
			TargetSpec: operation.TargetSpec{
				Racks: []operation.RackTarget{{
					Identifier: identifier.Identifier{ID: rackID},
				}},
			},
		})

		require.NoError(t, err)
		require.Equal(t, []uuid.UUID{taskID}, taskIDs)
		require.Zero(t, inventory.getRackCalls)
		require.Zero(t, store.createTaskCalls)
	})

	t.Run("rejects an idempotency key belonging to another rack", func(t *testing.T) {
		requestedRackID := uuid.New()
		existingRackID := uuid.New()
		idempotencyKey := "operation-run-target:" + uuid.NewString()
		store := &managerTaskStore{
			taskByIdempotencyKey: map[string]*taskdef.Task{
				idempotencyKey: {
					ID:             uuid.New(),
					RackID:         existingRackID,
					ExecutionID:    `{"workflow_id":"workflow","run_id":"run"}`,
					IdempotencyKey: idempotencyKey,
				},
			},
		}
		inventory := &submitTaskInventory{}
		manager := &ManagerImpl{inventoryStore: inventory, taskStore: store}

		taskIDs, err := manager.SubmitTask(context.Background(), &operation.Request{
			Operation:      testPowerControlOperation(t),
			RequiredRackID: requestedRackID,
			IdempotencyKey: idempotencyKey,
			TargetSpec: operation.TargetSpec{
				Racks: []operation.RackTarget{{
					Identifier: identifier.Identifier{ID: requestedRackID},
				}},
			},
		})

		require.Nil(t, taskIDs)
		require.ErrorContains(t, err, "idempotency key")
		require.ErrorContains(t, err, existingRackID.String())
		require.ErrorContains(t, err, requestedRackID.String())
		require.Zero(t, inventory.getRackCalls)
		require.Zero(t, store.createTaskCalls)
	})

	t.Run("rejects unlinked ingest with an actual inventory rule", func(t *testing.T) {
		rackID := uuid.New()
		ruleID := uuid.New()
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinked := newTestComponent(
			uuid.New(),
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)

		rule := &operationrules.OperationRule{
			ID:            ruleID,
			OperationType: taskcommon.TaskTypeBringUp,
			OperationCode: taskcommon.OpCodeIngest,
			RuleDefinition: operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{
				ComponentType: devicetypes.ComponentTypeCompute,
				Stage:         1,
				MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
			}}},
		}
		store := &managerTaskStore{rulesByID: map[uuid.UUID]*operationrules.OperationRule{ruleID: rule}}
		manager := &ManagerImpl{
			inventoryStore: &submitTaskInventory{rack: resolvedRack},
			taskStore:      store,
			ruleResolver:   operationrules.NewResolver(store),
		}

		_, err := manager.SubmitTask(context.Background(), &operation.Request{
			Operation: testIngestOperation(t, &ruleID),
			RuleID:    &ruleID,
			TargetSpec: operation.TargetSpec{
				Racks: []operation.RackTarget{{
					Identifier: identifier.Identifier{ID: rackID},
				}},
			},
		})

		require.ErrorContains(t, err, "selected components not linked to actual inventory (1)")
		require.Zero(t, store.createTaskCalls)
	})
}

func TestManagerImpl_ExecuteTask(t *testing.T) {
	t.Run("rejects an unlinked operation target", func(t *testing.T) {
		rackID := uuid.New()
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinked := newTestComponent(
			uuid.New(),
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)

		manager := &ManagerImpl{}
		resp, err := manager.executeTask(
			context.Background(),
			&taskdef.Task{
				ID:        uuid.New(),
				RackID:    rackID,
				Operation: testPowerControlOperation(t),
			},
			resolvedRack,
			&operationrules.RuleDefinition{},
		)

		require.Nil(t, resp)
		require.ErrorContains(t, err, "operation cannot be executed")
		require.ErrorContains(t, err, "selected components not linked to actual inventory (1)")
	})

	t.Run("rejects unlinked ingest with an actual inventory rule", func(t *testing.T) {
		rackID := uuid.New()
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinked := newTestComponent(
			uuid.New(),
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)
		ruleDef := &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{
			MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
		}}}

		resp, err := (&ManagerImpl{}).executeTask(
			context.Background(),
			&taskdef.Task{
				ID:        uuid.New(),
				RackID:    rackID,
				Operation: testIngestOperation(t, nil),
			},
			resolvedRack,
			ruleDef,
		)

		require.Nil(t, resp)
		require.ErrorContains(t, err, "operation cannot be executed")
		require.ErrorContains(t, err, "selected components not linked to actual inventory (1)")
	})
}

func TestManagerImpl_ResolveAndExecuteTask(t *testing.T) {
	t.Run("retries unlinked targets until the deadline", func(t *testing.T) {
		rackID := uuid.New()
		componentID := uuid.New()
		deadline := time.Now().Add(time.Hour)
		resolvedRack := newTestRack(rackID, "rack-1")
		component := newTestComponent(
			componentID,
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		component.ComponentID = ""
		resolvedRack.AddComponent(component)
		task := &taskdef.Task{
			ID:             uuid.New(),
			RackID:         rackID,
			Operation:      testPowerControlOperation(t),
			Status:         taskcommon.TaskStatusPending,
			QueueExpiresAt: &deadline,
		}
		store := &managerTaskStore{}
		executor := &managerExecutor{executionID: `{"workflow_id":"workflow","run_id":"run"}`}
		manager := &ManagerImpl{
			taskStore:    store,
			executor:     executor,
			ruleResolver: operationrules.NewResolver(store),
		}

		err := manager.resolveAndExecuteTask(context.Background(), task, resolvedRack)

		require.NoError(t, err)
		require.Equal(t, taskcommon.TaskStatusWaiting, task.Status)
		require.Len(t, store.statusUpdates, 1)
		require.Equal(t, taskcommon.TaskStatusWaiting, store.statusUpdates[0].Status)
		require.Equal(t, deadline, *store.statusUpdates[0].QueueExpiresAt)
		require.Equal(t, 1, store.runTransactionCalls)
		require.Equal(t, 1, store.lockRackCalls)
		require.Equal(t, 1, store.countWaitingCalls)
		require.Zero(t, executor.executeCalls)

		// A later promotion reloads the same task after inventory linkage recovers.
		task.Status = taskcommon.TaskStatusPending
		resolvedRack.Components[0].ComponentID = "machine-1"
		err = manager.resolveAndExecuteTask(context.Background(), task, resolvedRack)

		require.NoError(t, err)
		require.Equal(t, 1, executor.executeCalls)
		require.Equal(t, 1, store.updateScheduledCalls)
		require.Equal(t, executor.executionID, store.updatedScheduledTask.ExecutionID)
	})

	t.Run("terminates unlinked targets at the deadline", func(t *testing.T) {
		rackID := uuid.New()
		deadline := time.Now().Add(-time.Minute)
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinked := newTestComponent(
			uuid.New(),
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)
		task := &taskdef.Task{
			ID:             uuid.New(),
			RackID:         rackID,
			Operation:      testPowerControlOperation(t),
			Status:         taskcommon.TaskStatusPending,
			QueueExpiresAt: &deadline,
		}
		store := &managerTaskStore{}
		manager := &ManagerImpl{
			taskStore:    store,
			executor:     &managerExecutor{},
			ruleResolver: operationrules.NewResolver(store),
		}

		err := manager.resolveAndExecuteTask(context.Background(), task, resolvedRack)

		require.NoError(t, err)
		require.Equal(t, taskcommon.TaskStatusTerminated, task.Status)
		require.Len(t, store.statusUpdates, 1)
		require.Equal(t, taskcommon.TaskStatusTerminated, store.statusUpdates[0].Status)
		require.Nil(t, store.statusUpdates[0].QueueExpiresAt)
		require.Nil(t, task.QueueExpiresAt)
		require.Zero(t, store.runTransactionCalls)
	})

	t.Run("terminates when the waiting queue is full", func(t *testing.T) {
		rackID := uuid.New()
		deadline := time.Now().Add(time.Hour)
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinked := newTestComponent(
			uuid.New(),
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)
		task := &taskdef.Task{
			ID:             uuid.New(),
			RackID:         rackID,
			Operation:      testPowerControlOperation(t),
			Status:         taskcommon.TaskStatusPending,
			QueueExpiresAt: &deadline,
		}
		store := &managerTaskStore{waitingCount: 1}
		manager := &ManagerImpl{
			taskStore:         store,
			executor:          &managerExecutor{},
			ruleResolver:      operationrules.NewResolver(store),
			maxWaitingPerRack: 1,
		}

		err := manager.resolveAndExecuteTask(context.Background(), task, resolvedRack)

		require.NoError(t, err)
		require.Equal(t, taskcommon.TaskStatusTerminated, task.Status)
		require.Nil(t, task.QueueExpiresAt)
		require.Len(t, store.statusUpdates, 1)
		require.Equal(t, taskcommon.TaskStatusTerminated, store.statusUpdates[0].Status)
		require.Nil(t, store.statusUpdates[0].QueueExpiresAt)
		require.Contains(t, task.Message, "waiting queue is full while target linkage is unavailable (1/1 tasks)")
		require.Equal(t, 1, store.runTransactionCalls)
		require.Equal(t, 1, store.lockRackCalls)
		require.Equal(t, 1, store.countWaitingCalls)
	})
}

func TestManagerImpl_CreateAndExecuteTask(t *testing.T) {
	t.Run("waits when a target unlinks after admission", func(t *testing.T) {
		rackID := uuid.New()
		resolvedRack := newTestRack(rackID, "rack-1")
		unlinked := newTestComponent(
			uuid.New(),
			rackID,
			devicetypes.ComponentTypeCompute,
			"compute-1",
		)
		unlinked.ComponentID = ""
		resolvedRack.AddComponent(unlinked)
		store := &managerTaskStore{}
		executor := &managerExecutor{}
		manager := &ManagerImpl{
			taskStore:           store,
			executor:            executor,
			ruleResolver:        operationrules.NewResolver(store),
			conflictResolver:    conflict.NewResolver(store),
			defaultQueueTimeout: 30 * time.Minute,
		}
		startedAt := time.Now()

		taskID, err := manager.createAndExecuteTask(context.Background(), &operation.Request{
			Operation:        testPowerControlOperation(t),
			ConflictStrategy: operation.ConflictStrategyReject,
		}, resolvedRack)

		require.NoError(t, err)
		require.NotEqual(t, uuid.Nil, taskID)
		require.Equal(t, 1, store.createTaskCalls)
		require.Len(t, store.statusUpdates, 1)
		update := store.statusUpdates[0]
		require.Equal(t, taskID, update.ID)
		require.Equal(t, taskcommon.TaskStatusWaiting, update.Status)
		require.NotNil(t, update.QueueExpiresAt)
		require.WithinDuration(
			t,
			startedAt.Add(manager.defaultQueueTimeout),
			*update.QueueExpiresAt,
			time.Second,
		)
		require.Zero(t, executor.executeCalls)
	})
}

func TestValidateResolvedRackTargets(t *testing.T) {
	tests := []struct {
		name         string
		op           operation.Wrapper
		ruleDef      *operationrules.RuleDefinition
		componentIDs []string
		wantError    string
	}{
		{
			name: "linked components allow a disruptive operation",
			op: operation.Wrapper{
				Type: taskcommon.TaskTypePowerControl,
				Code: taskcommon.OpCodePowerControlPowerOff,
			},
			componentIDs: []string{"machine-1", "machine-2"},
		},
		{
			name: "unlinked component rejects a disruptive operation",
			op: operation.Wrapper{
				Type: taskcommon.TaskTypeFirmwareControl,
				Code: taskcommon.OpCodeFirmwareControlUpgrade,
			},
			componentIDs: []string{"machine-1", ""},
			wantError:    "selected components not linked to actual inventory (1)",
		},
		{
			name: "ingestion allows an unlinked expected component",
			op: operation.Wrapper{
				Type: taskcommon.TaskTypeBringUp,
				Code: taskcommon.OpCodeIngest,
			},
			ruleDef: &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{
				MainOperation: operationrules.ActionConfig{Name: operationrules.ActionInjectExpectation},
			}}},
			componentIDs: []string{""},
		},
		{
			name: "ingestion with an actual inventory action rejects an unlinked component",
			op: operation.Wrapper{
				Type: taskcommon.TaskTypeBringUp,
				Code: taskcommon.OpCodeIngest,
			},
			ruleDef: &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{
				MainOperation: operationrules.ActionConfig{Name: operationrules.ActionPowerControl},
			}}},
			componentIDs: []string{""},
			wantError:    "selected components not linked to actual inventory (1)",
		},
		{
			name: "inject expectation allows an unlinked expected component",
			op: operation.Wrapper{
				Type: taskcommon.TaskTypeInjectExpectation,
				Code: taskcommon.OpCodeInjectExpectation,
			},
			componentIDs: []string{""},
		},
		{
			name: "empty selected scope rejects every operation",
			op: operation.Wrapper{
				Type: taskcommon.TaskTypeBringUp,
				Code: taskcommon.OpCodeIngest,
			},
			wantError: "racks have no selected components",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			rackID := uuid.New()
			resolvedRack := newTestRack(rackID, "rack-1")
			componentFlowIDs := make([]uuid.UUID, 0, len(test.componentIDs))
			for i, externalID := range test.componentIDs {
				flowID := uuid.New()
				componentFlowIDs = append(componentFlowIDs, flowID)
				comp := newTestComponent(
					flowID,
					rackID,
					devicetypes.ComponentTypeCompute,
					fmt.Sprintf("compute-%d", i),
				)
				comp.ComponentID = externalID
				resolvedRack.AddComponent(comp)
			}

			err := validateResolvedRackTargets(
				test.op,
				test.ruleDef,
				map[uuid.UUID]*rack.Rack{rackID: resolvedRack},
			)
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				for i, externalID := range test.componentIDs {
					if externalID == "" {
						require.ErrorContains(t, err, componentFlowIDs[i].String())
					}
				}
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestManagerImpl_CreateAndExecuteIdempotentTask(t *testing.T) {
	t.Run("returns an existing scheduled task before rack conflict", func(t *testing.T) {
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
	})

	t.Run("schedules an existing pending task without an execution ID", func(t *testing.T) {
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
	})

	t.Run("rejects an existing task for a different rack", func(t *testing.T) {
		requestedRackID := uuid.New()
		existingRackID := uuid.New()
		idempotencyKey := "operation-run-target:" + uuid.NewString()
		targetRack := newTestRack(requestedRackID, "rack-1")
		store := &managerTaskStore{
			taskByIdempotencyKey: map[string]*taskdef.Task{
				idempotencyKey: {
					ID:             uuid.New(),
					RackID:         existingRackID,
					IdempotencyKey: idempotencyKey,
				},
			},
		}
		manager := &ManagerImpl{taskStore: store}

		taskID, err := manager.createAndExecuteTask(context.Background(), &operation.Request{
			Operation:      testPowerControlOperation(t),
			RequiredRackID: requestedRackID,
			IdempotencyKey: idempotencyKey,
		}, targetRack)

		require.Equal(t, uuid.Nil, taskID)
		require.ErrorContains(t, err, "idempotency key")
		require.ErrorContains(t, err, existingRackID.String())
		require.ErrorContains(t, err, requestedRackID.String())
		require.Equal(t, 1, store.lockKeyCalls)
		require.Zero(t, store.lockRackCalls)
		require.Zero(t, store.createTaskCalls)
		require.Zero(t, store.updateScheduledCalls)
	})

	t.Run("serializes concurrent retries until execution is persisted", func(t *testing.T) {
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
		baseStore := &managerTaskStore{
			taskByIdempotencyKey: map[string]*taskdef.Task{
				idempotencyKey: {
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
				},
			},
		}
		transactionAttempts := make(chan struct{}, 2)
		store := &serialManagerTaskStore{
			managerTaskStore:    baseStore,
			transactionAttempts: transactionAttempts,
		}
		executor := &blockingManagerExecutor{
			managerExecutor: managerExecutor{
				executionID: `{"workflow_id":"workflow","run_id":"run"}`,
			},
			started: make(chan struct{}),
			release: make(chan struct{}),
		}
		manager := &ManagerImpl{
			taskStore: store,
			executor:  executor,
		}
		req := &operation.Request{
			Operation:        op,
			ConflictStrategy: operation.ConflictStrategyReject,
			RequiredRackID:   rackID,
			IdempotencyKey:   idempotencyKey,
		}

		results := make(chan uuid.UUID, 2)
		errs := make(chan error, 2)
		var submissions sync.WaitGroup
		submissions.Add(2)
		submit := func() {
			defer submissions.Done()
			gotTaskID, err := manager.createAndExecuteTask(t.Context(), req, targetRack)
			results <- gotTaskID
			errs <- err
		}

		go submit()
		<-transactionAttempts
		<-executor.started
		go submit()
		<-transactionAttempts
		close(executor.release)
		submissions.Wait()
		close(results)
		close(errs)

		for err := range errs {
			require.NoError(t, err)
		}
		for gotTaskID := range results {
			require.Equal(t, taskID, gotTaskID)
		}
		require.EqualValues(t, 1, executor.calls.Load())
		require.Equal(t, 1, baseStore.updateScheduledCalls)
	})
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

func testIngestOperation(t *testing.T, ruleID *uuid.UUID) operation.Wrapper {
	t.Helper()

	info := &operations.BringUpTaskInfo{OpCode: taskcommon.OpCodeIngest}
	if ruleID != nil {
		info.RuleID = ruleID.String()
	}
	raw, err := info.Marshal()
	require.NoError(t, err)

	return operation.Wrapper{
		Type: taskcommon.TaskTypeBringUp,
		Code: taskcommon.OpCodeIngest,
		Info: raw,
	}
}

type managerTaskStore struct {
	activeTasksByRack    map[uuid.UUID][]*taskdef.Task
	taskByIdempotencyKey map[string]*taskdef.Task
	listActiveCalls      int
	createTaskCalls      int
	lockKeyCalls         int
	lockRackCalls        int
	updateScheduledCalls int
	updatedScheduledTask *taskdef.Task
	statusUpdates        []*taskdef.TaskStatusUpdate
	runTransactionCalls  int
	countWaitingCalls    int
	waitingCount         int
	rulesByID            map[uuid.UUID]*operationrules.OperationRule
}

type serialManagerTaskStore struct {
	*managerTaskStore
	idempotencyMu       sync.Mutex
	transactionAttempts chan<- struct{}
}

func (s *serialManagerTaskStore) RunInTransaction(
	ctx context.Context,
	fn func(context.Context) error,
) error {
	s.transactionAttempts <- struct{}{}
	err := fn(ctx)
	s.idempotencyMu.Unlock()
	return err
}

func (s *serialManagerTaskStore) LockIdempotencyKey(_ context.Context, _ string) error {
	s.idempotencyMu.Lock()
	return nil
}

func (s *serialManagerTaskStore) UpdateScheduledTask(
	ctx context.Context,
	task *taskdef.Task,
) error {
	err := s.managerTaskStore.UpdateScheduledTask(ctx, task)
	if err != nil {
		return err
	}

	persisted := *task
	s.taskByIdempotencyKey[task.IdempotencyKey] = &persisted
	return nil
}

func (s *managerTaskStore) RunInTransaction(
	ctx context.Context,
	fn func(context.Context) error,
) error {
	s.runTransactionCalls++
	return fn(ctx)
}

func (s *managerTaskStore) CreateTask(_ context.Context, _ *taskdef.Task) error {
	s.createTaskCalls++
	return nil
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

func (s *managerTaskStore) GetTask(_ context.Context, _ uuid.UUID) (*taskdef.Task, error) {
	panic("managerTaskStore.GetTask: not implemented")
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
	s.countWaitingCalls++
	return s.waitingCount, nil
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
	id uuid.UUID,
) (*operationrules.OperationRule, error) {
	return s.rulesByID[id], nil
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
	return nil, nil
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
	executionID  string
	executeCalls int
	lastRequest  *taskdef.ExecutionRequest
}

type blockingManagerExecutor struct {
	managerExecutor
	calls   atomic.Int32
	started chan struct{}
	release chan struct{}
}

func (e *blockingManagerExecutor) Execute(
	_ context.Context,
	_ *taskdef.ExecutionRequest,
) (*taskdef.ExecutionResponse, error) {
	if e.calls.Add(1) == 1 {
		close(e.started)
	}
	<-e.release
	return &taskdef.ExecutionResponse{ExecutionID: e.executionID}, nil
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
	panic("managerExecutor.TerminateTask: not implemented")
}
