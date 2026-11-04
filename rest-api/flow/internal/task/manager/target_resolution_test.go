// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"encoding/json"
	"os"
	"testing"

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

func TestManagerImpl_ResolveSubmissionTargets(t *testing.T) {
	for _, tc := range []struct {
		name      string
		mode      taskcommon.TargetResolutionMode
		linked    bool
		filter    bool
		wantError string
	}{
		{name: "omitted is strict", linked: true, wantError: "not linked"},
		{name: "explicit strict", mode: taskcommon.TargetResolutionStrict, linked: true, wantError: "not linked"},
		{name: "best effort partitions targets", mode: taskcommon.TargetResolutionBestEffort, linked: true},
		{name: "best effort rejects no actionable targets", mode: taskcommon.TargetResolutionBestEffort, wantError: "no addressable"},
		{name: "type filter precedes strict validation", linked: true, filter: true},
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
			req := &operation.Request{Operation: testPowerControlOperation(t), TargetResolutionMode: tc.mode, TargetSpec: operation.TargetSpec{Racks: []operation.RackTarget{target}}}
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
			if tc.mode == taskcommon.TargetResolutionBestEffort {
				require.Len(t, resolution.Skipped, 1)
				require.Equal(t, missing.Info.ID, resolution.Skipped[0].ComponentID)
				require.Equal(t, 9, resolution.Skipped[0].SlotID)
			}
		})
	}
}

func TestManagerImpl_BestEffortQueuedSnapshot(t *testing.T) {
	rackID := uuid.New()
	selected := newTestRack(rackID, "rack-1")
	linked := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "compute-1")
	missing := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "")
	missing.ComponentID = ""
	selected.AddComponent(linked)
	selected.AddComponent(missing)
	store := &managerTaskStore{activeTasksByRack: map[uuid.UUID][]*taskdef.Task{rackID: {{ID: uuid.New(), RackID: rackID, Operation: testPowerControlOperation(t), Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {linked.Info.ID}}}, Status: taskcommon.TaskStatusRunning}}}}
	exec := &managerExecutor{executionID: "workflow-1"}
	inv := &submitTaskInventory{rack: selected}
	m := &ManagerImpl{inventoryStore: inv, taskStore: store, executor: exec, ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store), maxWaitingPerRack: 5}
	req := &operation.Request{Operation: testPowerControlOperation(t), TargetResolutionMode: taskcommon.TargetResolutionBestEffort, ConflictStrategy: operation.ConflictStrategyQueue, TargetSpec: operation.TargetSpec{Racks: []operation.RackTarget{{Identifier: identifier.Identifier{ID: rackID}}}}}
	ids, err := m.SubmitTask(t.Context(), req)
	require.NoError(t, err)
	require.Len(t, ids, 1)
	require.Zero(t, exec.executeCalls)
	require.Len(t, store.createdTasks, 1)
	// Persist/reload the JSON attributes so the proof covers the stored contract.
	original := store.createdTasks[0]
	raw, err := json.Marshal(original.Attributes)
	require.NoError(t, err)
	var attrs taskcommon.TaskAttributes
	require.NoError(t, json.Unmarshal(raw, &attrs))
	original.Attributes = attrs
	rep, err := report.Unmarshal(original.Report)
	require.NoError(t, err)
	require.Equal(t, 1, rep.TargetResolution.Submitted)
	require.Len(t, rep.TargetResolution.Skipped, 1)
	selected.Components[1].ComponentID = "newly-linked"
	selected.AddComponent(newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "new-device"))
	original.Status = taskcommon.TaskStatusPending
	store.tasksByID = map[uuid.UUID]*taskdef.Task{original.ID: original}
	require.NoError(t, m.promoteTask(t.Context(), original.ID))
	require.Len(t, exec.lastRequest.Info.Components, 1)
	require.Equal(t, "compute-1", exec.lastRequest.Info.Components[0].ComponentID)
	require.Equal(t, attrs.TargetResolution, exec.lastRequest.Info.TargetResolution)
	// Recover an admitted but unscheduled task using the same idempotency key.
	// Newly linked/skipped inventory must not be re-planned on that retry.
	original.ExecutionID = ""
	original.IdempotencyKey = "pending-recovery"
	store.taskByIdempotencyKey = map[string]*taskdef.Task{original.IdempotencyKey: original}
	req.IdempotencyKey = original.IdempotencyKey
	req.RequiredRackID = rackID
	recovered, err := m.SubmitTask(t.Context(), req)
	require.NoError(t, err)
	require.Equal(t, []uuid.UUID{original.ID}, recovered)
	require.Len(t, exec.lastRequest.Info.Components, 1)
	require.Equal(t, attrs.TargetResolution, exec.lastRequest.Info.TargetResolution)
	// A component disappearing cannot silently shrink the accepted set.
	selected.Components = selected.Components[1:]
	err = m.promoteTask(t.Context(), original.ID)
	require.ErrorContains(t, err, "no longer present")
	require.Equal(t, taskcommon.TaskStatusFailed, store.statusUpdates[len(store.statusUpdates)-1].Status)
}

func TestManagerImpl_SubmitRackBatch(t *testing.T) {
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
		resolution := &taskcommon.TargetResolution{Mode: taskcommon.TargetResolutionBestEffort, Selected: 2, Submitted: 1, Skipped: []taskcommon.SkippedTarget{{ComponentID: uuid.New(), ComponentType: devicetypes.ComponentTypeCompute, Reason: "unaddressable_target"}}}
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
