// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/report"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

func TestManagerImpl_ValidateTask(t *testing.T) {
	for _, tc := range []struct {
		name      string
		skip      bool
		linked    bool
		filter    bool
		wantError string
	}{
		{name: "default rejects unaddressable targets", linked: true, wantError: "not linked"},
		{name: "opt in partitions targets", skip: true, linked: true},
		{name: "opt in rejects no actionable targets", skip: true, wantError: "no addressable"},
		{name: "type filter precedes addressability validation", linked: true, filter: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rackID := uuid.New()
			selected := newTestRack(rackID, "rack-1")
			if tc.linked {
				selected.AddComponent(newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "compute-1"))
			}
			missing := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch, "")
			missing.ComponentID = ""
			missing.Position = component.InRackPosition{SlotID: 9, TrayIndex: 2, HostID: 1}
			selected.AddComponent(missing)
			store := &managerTaskStore{}
			m := &ManagerImpl{inventoryStore: &submitTaskInventory{rack: selected}, taskStore: store, ruleResolver: operationrules.NewResolver(store)}
			target := operation.RackTarget{Identifier: identifier.Identifier{ID: rackID}}
			if tc.filter {
				target.ComponentTypes = []devicetypes.ComponentType{devicetypes.ComponentTypeCompute}
			}
			req := &operation.Request{Operation: testPowerControlOperation(t), SkipUnaddressableTargets: tc.skip, TargetSpec: operation.TargetSpec{Racks: []operation.RackTarget{target}}}
			racks, resolutions, err := m.validateTask(t.Context(), req)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				require.Zero(t, store.createTaskCalls)
				return
			}
			require.NoError(t, err)
			require.Len(t, racks[rackID].Components, 1)
			resolution := resolutions[rackID]
			require.Equal(t, resolution.Selected, resolution.Submitted+len(resolution.Skipped))
			if tc.skip {
				require.Len(t, resolution.Skipped, 1)
				require.Equal(t, missing.Info.ID, resolution.Skipped[0].ComponentID)
				require.Equal(t, 9, resolution.Skipped[0].SlotID)
			}
		})
	}
}

func TestManagerImpl_SubmitRackBatch(t *testing.T) {
	for _, tc := range []struct {
		name             string
		cancelSecond     bool
		statusWriteFails bool
	}{
		{name: "cancellation during an earlier rack RPC is respected", cancelSecond: true},
		{name: "status-write failure cannot hide or starve admitted siblings", statusWriteFails: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rackIDs := []uuid.UUID{uuid.MustParse("00000000-0000-0000-0000-000000000001"), uuid.MustParse("00000000-0000-0000-0000-000000000002")}
			racks := map[uuid.UUID]*rack.Rack{}
			for _, id := range rackIDs {
				r := newTestRack(id, "rack")
				r.AddComponent(newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, "compute"))
				racks[id] = r
			}
			store := &managerTaskStore{}
			exec := &managerExecutor{executionID: "workflow"}
			if tc.cancelSecond {
				exec.onExecute = func(_ *taskdef.ExecutionRequest) { store.createdTasks[1].Status = taskcommon.TaskStatusTerminated }
			}
			if tc.statusWriteFails {
				exec.executeErr = errors.New("backend unavailable")
				store.statusUpdateErr = errors.New("status write unavailable")
			}
			m := &ManagerImpl{taskStore: store, executor: exec, ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			ids, err := m.submitRackBatch(t.Context(), &operation.Request{Operation: testPowerControlOperation(t)}, racks, nil)
			require.NoError(t, err)
			require.Len(t, ids, 2)
			if tc.cancelSecond {
				require.Equal(t, 1, exec.executeCalls)
				saved, err := store.GetTask(t.Context(), ids[1])
				require.NoError(t, err)
				require.Equal(t, taskcommon.TaskStatusTerminated, saved.Status)
			} else {
				require.Equal(t, 2, exec.executeCalls)
			}
		})
	}

	t.Run("caller cancellation preserves durable dispatch ownership", func(t *testing.T) {
		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()
		racks := make(map[uuid.UUID]*rack.Rack)
		for range 2 {
			id := uuid.New()
			r := newTestRack(id, "rack")
			r.AddComponent(newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, "compute"))
			racks[id] = r
		}
		store := &contextAwareBatchStore{managerTaskStore: &managerTaskStore{}, t: t}
		exec := &managerExecutor{executeErr: context.Canceled, onExecute: func(_ *taskdef.ExecutionRequest) { cancel() }}
		m := &ManagerImpl{taskStore: store, executor: exec, ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
		ids, err := m.submitRackBatch(ctx, &operation.Request{Operation: testPowerControlOperation(t)}, racks, nil)
		require.NoError(t, err)
		require.Len(t, ids, 2)
		for i, id := range ids {
			saved, err := store.GetTask(t.Context(), id)
			require.NoError(t, err)
			if i == 0 {
				require.Equal(t, taskcommon.TaskStatusFailed, saved.Status)
			} else {
				require.Equal(t, taskcommon.TaskStatusPending, saved.Status)
				require.Equal(t, taskcommon.BatchDispatchReady, saved.Attributes.BatchDispatch)
			}
			require.Empty(t, saved.ExecutionID)
		}
	})

	t.Run("read failures are recovered by the persisted waiting queue", func(t *testing.T) {
		if os.Getenv("DB_PORT") == "" {
			t.Skip("requires runner-managed PostgreSQL")
		}
		config, err := cdb.ConfigFromEnv()
		require.NoError(t, err)
		session, err := utils.UnitTestDB(t.Context(), t, config)
		require.NoError(t, err)
		t.Cleanup(session.Close)
		persisted := taskstore.NewPostgres(session)
		store := &failingTaskReadStore{Store: persisted, failures: 1}
		rackIDs := []uuid.UUID{uuid.MustParse("00000000-0000-0000-0000-000000000001"), uuid.MustParse("00000000-0000-0000-0000-000000000002")}
		racks := make(map[uuid.UUID]*rack.Rack)
		resolutions := make(map[uuid.UUID]*taskcommon.TargetResolution)
		for _, id := range rackIDs {
			r := newTestRack(id, "rack")
			r.AddComponent(newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, "compute"))
			racks[id] = r
			resolutions[id] = &taskcommon.TargetResolution{SkipUnaddressableTargets: true, Selected: 2, Submitted: 1,
				Skipped: []taskcommon.SkippedTarget{{ComponentID: uuid.New(), Reason: "unaddressable_target"}}}
		}
		requests := make(chan taskdef.ExecutionRequest, 2)
		exec := &managerExecutor{executionID: "workflow", onExecute: func(req *taskdef.ExecutionRequest) { requests <- *req }}
		m := &ManagerImpl{inventoryStore: &submitTaskInventory{rack: racks[rackIDs[0]]}, taskStore: store, executor: exec,
			ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
		ids, err := m.submitRackBatch(t.Context(), &operation.Request{Operation: testPowerControlOperation(t), QueueTimeout: time.Minute}, racks, resolutions)
		require.NoError(t, err)
		require.Len(t, ids, 2)
		saved, err := persisted.GetTask(t.Context(), ids[0])
		require.NoError(t, err)
		require.Equal(t, taskcommon.TaskStatusWaiting, saved.Status)
		require.Empty(t, saved.ExecutionID)
		require.NotNil(t, saved.QueueExpiresAt)
		deadline := *saved.QueueExpiresAt
		waitingRacks, listErr := persisted.ListRacksWithWaitingTasks(t.Context())
		require.NoError(t, listErr)
		require.Contains(t, waitingRacks, rackIDs[0])
		// Fail the first promotion read as well; the next sweep must retry it.
		store.failures = 1
		ctx, cancel := context.WithCancel(t.Context())
		t.Cleanup(cancel)
		promoter := conflict.NewPromoter(store, m.promoteTask, conflict.PromoterConfig{SweepInterval: 10 * time.Millisecond})
		promoter.Start(ctx)
		require.Eventually(t, func() bool {
			current, loadErr := persisted.GetTask(t.Context(), ids[0])
			return loadErr == nil && current.IsScheduled()
		}, 5*time.Second, 10*time.Millisecond)
		cancel()
		require.Len(t, requests, 2)
		for range 2 {
			req := <-requests
			require.Len(t, req.Info.Components, 1)
			require.Equal(t, 1, req.Info.TargetResolution.Submitted)
			require.Len(t, req.Info.TargetResolution.Skipped, 1)
		}
		saved, err = persisted.GetTask(t.Context(), ids[0])
		require.NoError(t, err)
		require.Equal(t, deadline, *saved.QueueExpiresAt)
		require.Equal(t, resolutions[rackIDs[0]], saved.Attributes.TargetResolution)
	})

	t.Run("restart recovers only batch members never attempted", func(t *testing.T) {
		if os.Getenv("DB_PORT") == "" {
			t.Skip("requires runner-managed PostgreSQL")
		}
		config, err := cdb.ConfigFromEnv()
		require.NoError(t, err)
		session, err := utils.UnitTestDB(t.Context(), t, config)
		require.NoError(t, err)
		t.Cleanup(session.Close)
		store := taskstore.NewPostgres(session)
		rackIDs := []uuid.UUID{uuid.MustParse("00000000-0000-0000-0000-000000000001"), uuid.MustParse("00000000-0000-0000-0000-000000000002")}
		racks := make(map[uuid.UUID]*rack.Rack)
		resolutions := make(map[uuid.UUID]*taskcommon.TargetResolution)
		for _, id := range rackIDs {
			r := newTestRack(id, "rack")
			r.AddComponent(newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, "compute"))
			racks[id] = r
			resolutions[id] = &taskcommon.TargetResolution{SkipUnaddressableTargets: true, Selected: 2, Submitted: 1,
				Skipped: []taskcommon.SkippedTarget{{ComponentID: uuid.New(), Reason: "unaddressable_target"}}}
		}
		exec := &managerExecutor{onExecute: func(*taskdef.ExecutionRequest) { panic("simulate process exit after external submission") }}
		m := &ManagerImpl{taskStore: store, executor: exec, ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
		require.Panics(t, func() {
			_, _ = m.submitRackBatch(t.Context(), &operation.Request{Operation: testPowerControlOperation(t)}, racks, resolutions)
		})
		ready, err := store.ListPendingBatchDispatches(t.Context())
		require.NoError(t, err)
		require.Len(t, ready, 1)
		require.Equal(t, rackIDs[1], ready[0].RackID)
		requests := make(chan taskdef.ExecutionRequest, 1)
		restarted := &ManagerImpl{taskStore: store, inventoryStore: &submitTaskInventory{rack: racks[rackIDs[1]]},
			executor:     &managerExecutor{executionID: "recovered", onExecute: func(req *taskdef.ExecutionRequest) { requests <- *req }},
			ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
		restarted.promoter = conflict.NewPromoter(store, restarted.promoteTask, conflict.PromoterConfig{})
		ctx, cancel := context.WithCancel(t.Context())
		t.Cleanup(cancel)
		require.NoError(t, restarted.Start(ctx))
		require.Eventually(t, func() bool {
			saved, loadErr := store.GetTask(t.Context(), ready[0].ID)
			return loadErr == nil && saved.ExecutionID == "recovered"
		}, 5*time.Second, 10*time.Millisecond)
		cancel()
		require.Len(t, requests, 1)
		req := <-requests
		require.Equal(t, ready[0].ID, req.Info.TaskID)
		require.Equal(t, resolutions[rackIDs[1]], req.Info.TargetResolution)
		require.Len(t, req.Info.Components, 1)
		remaining, err := store.ListPendingBatchDispatches(t.Context())
		require.NoError(t, err)
		require.Empty(t, remaining)
	})

	t.Run("postgres rolls back earlier rack admission", func(t *testing.T) {
		if os.Getenv("DB_PORT") == "" {
			t.Skip("requires runner-managed PostgreSQL")
		}
		config, err := cdb.ConfigFromEnv()
		require.NoError(t, err)
		session, err := utils.UnitTestDB(t.Context(), t, config)
		require.NoError(t, err)
		t.Cleanup(session.Close)
		store := taskstore.NewPostgres(session)
		rackIDs := []uuid.UUID{uuid.MustParse("00000000-0000-0000-0000-000000000001"), uuid.MustParse("00000000-0000-0000-0000-000000000002")}
		racks := map[uuid.UUID]*rack.Rack{}
		for _, id := range rackIDs {
			r := newTestRack(id, "rack")
			r.AddComponent(newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, "compute"))
			racks[id] = r
		}
		req := &operation.Request{Operation: testPowerControlOperation(t)}
		active := newTaskForRack(req, racks[rackIDs[1]])
		active.Status = taskcommon.TaskStatusRunning
		require.NoError(t, store.CreateTask(t.Context(), &active))
		exec := &managerExecutor{}
		m := &ManagerImpl{taskStore: store, executor: exec, conflictResolver: conflict.NewResolver(store)}
		got, err := m.submitRackBatch(t.Context(), req, racks, nil)
		require.ErrorIs(t, err, ErrRackConflict)
		require.Nil(t, got)
		require.Zero(t, exec.executeCalls)
		var count int
		require.NoError(t, session.DB.NewRaw("SELECT COUNT(*) FROM task WHERE rack_id = ?", rackIDs[0]).Scan(t.Context(), &count))
		require.Zero(t, count)
		// Attributes/report are additive JSON; predecessor rows still decode.
		legacy, err := store.GetTask(t.Context(), active.ID)
		require.NoError(t, err)
		require.Nil(t, legacy.Attributes.TargetResolution)
		resolution := &taskcommon.TargetResolution{SkipUnaddressableTargets: true, Selected: 2, Submitted: 1, Skipped: []taskcommon.SkippedTarget{{ComponentID: uuid.New(), ComponentType: devicetypes.ComponentTypeCompute, Reason: "unaddressable_target"}}}
		snapshot := newTaskForRack(req, racks[rackIDs[0]], resolution)
		snapshot.Status = taskcommon.TaskStatusWaiting
		require.NoError(t, initializeTargetReport(&snapshot))
		require.NoError(t, store.CreateTask(t.Context(), &snapshot))
		saved, err := store.GetTask(t.Context(), snapshot.ID)
		require.NoError(t, err)
		require.Equal(t, resolution, saved.Attributes.TargetResolution)
		rep, err := report.Unmarshal(saved.Report)
		require.NoError(t, err)
		require.Equal(t, resolution, rep.TargetResolution)
	})

	for _, conflictOnSecond := range []bool{false, true} {
		t.Run(map[bool]string{false: "admits all records before dispatch", true: "admission failure dispatches nothing"}[conflictOnSecond], func(t *testing.T) {
			ids := []uuid.UUID{uuid.MustParse("00000000-0000-0000-0000-000000000001"), uuid.MustParse("00000000-0000-0000-0000-000000000002")}
			racks := map[uuid.UUID]*rack.Rack{}
			for _, id := range ids {
				r := newTestRack(id, "rack")
				r.AddComponent(newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, "compute"))
				racks[id] = r
			}
			store := &managerTaskStore{}
			if conflictOnSecond {
				active := newTaskForRack(&operation.Request{Operation: testPowerControlOperation(t)}, racks[ids[1]])
				active.Status = taskcommon.TaskStatusRunning
				store.activeTasksByRack = map[uuid.UUID][]*taskdef.Task{ids[1]: {&active}}
			}
			exec := &managerExecutor{executionID: "workflow"}
			m := &ManagerImpl{taskStore: store, executor: exec, ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			got, err := m.submitRackBatch(t.Context(), &operation.Request{Operation: testPowerControlOperation(t)}, racks, nil)
			if conflictOnSecond {
				require.ErrorIs(t, err, ErrRackConflict)
				require.Nil(t, got)
				require.Zero(t, exec.executeCalls)
				return
			}
			require.NoError(t, err)
			require.Len(t, got, 2)
			require.Equal(t, 1, store.runTransactionCalls)
			require.Equal(t, 2, store.createTaskCalls)
			require.Equal(t, 2, exec.executeCalls)
		})
	}
}

// contextAwareBatchStore models cancellation-sensitive database reads and writes.
type contextAwareBatchStore struct {
	*managerTaskStore
	t *testing.T
}

func (s *contextAwareBatchStore) GetTask(ctx context.Context, id uuid.UUID) (*taskdef.Task, error) {
	err := ctx.Err()
	if err != nil {
		return nil, err
	}
	return s.managerTaskStore.GetTask(ctx, id)
}

func (s *contextAwareBatchStore) UpdateTaskStatus(ctx context.Context, update *taskdef.TaskStatusUpdate) error {
	err := ctx.Err()
	if err != nil {
		return err
	}
	_, bounded := ctx.Deadline()
	require.True(s.t, bounded)
	err = s.managerTaskStore.UpdateTaskStatus(ctx, update)
	if err != nil {
		return err
	}
	s.tasksByID[update.ID].Status = update.Status
	return nil
}

// failingTaskReadStore injects transient reads while retaining real persistence.
type failingTaskReadStore struct {
	taskstore.Store
	failures int
}

func (s *failingTaskReadStore) GetTask(ctx context.Context, id uuid.UUID) (*taskdef.Task, error) {
	if s.failures > 0 {
		s.failures--
		return nil, errors.New("temporary task read failure")
	}
	return s.Store.GetTask(ctx, id)
}
