// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func testNVOSRestartSubmission(t *testing.T) {
	for _, tc := range []struct {
		name     string
		targets  []string
		snapshot bool
		missing  bool
		unlinked bool
		conflict bool
		legacy   bool
	}{
		{name: "NVOS snapshots full compute scope", targets: []string{"nvos"}, snapshot: true},
		{name: "whole bundle includes NVOS", snapshot: true},
		{name: "BMC-only retains original scope", targets: []string{"bmc"}},
		{name: "missing reserved compute fails recovery", targets: []string{"nvos"}, snapshot: true, missing: true},
		{name: "unaddressable compute rejects before persistence", targets: []string{"nvos"}, unlinked: true},
		{name: "compute power task blocks switch NVOS update", targets: []string{"nvos"}, conflict: true},
		{name: "predecessor task retains firmware-only behavior", targets: []string{"nvos"}, snapshot: true, legacy: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rackID, switchID, computeID := uuid.New(), uuid.New(), uuid.New()
			fullRack := newTestRack(rackID, "rack-1")
			sw := newTestComponent(switchID, rackID, devicetypes.ComponentTypeNVSwitch, "switch-1")
			compute := newTestComponent(computeID, rackID, devicetypes.ComponentTypeCompute, "compute-1")
			if tc.unlinked {
				compute.ComponentID = ""
			}
			fullRack.AddComponent(sw)
			fullRack.AddComponent(compute)
			inv := &submitTaskInventory{rack: fullRack, components: map[uuid.UUID]*component.Component{switchID: sw}}
			store := &managerTaskStore{}
			exec := &managerExecutor{executionID: "execution-1"}
			m := &ManagerImpl{inventoryStore: inv, taskStore: store, executor: exec,
				ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			raw, err := (&operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: tc.targets}).Marshal()
			require.NoError(t, err)
			if tc.conflict {
				active := newTaskForRack(&operation.Request{Operation: testPowerControlOperation(t)}, fullRack)
				store.activeTasksByRack = map[uuid.UUID][]*taskdef.Task{rackID: {&active}}
			}
			ids, err := m.SubmitTask(context.Background(), &operation.Request{
				Operation:  operation.Wrapper{Type: taskcommon.TaskTypeFirmwareControl, Code: taskcommon.OpCodeFirmwareControlUpgrade, Info: raw},
				TargetSpec: operation.TargetSpec{Components: []operation.ComponentTarget{{UUID: switchID}}},
			})
			require.NoError(t, err)
			if tc.unlinked || tc.conflict {
				require.Empty(t, ids)
				require.Zero(t, store.createTaskCalls)
				require.Zero(t, exec.executeCalls)
				return
			}
			require.Len(t, ids, 1)
			require.Equal(t, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeNVSwitch, ComponentID: "switch-1"}}, exec.lastRequest.Info.Components)
			saved := *store.updatedScheduledTask
			// Exercise the JSONB representation, then recover through the same path
			// as a queued task after inventory has changed.
			data, err := json.Marshal(saved.Attributes)
			require.NoError(t, err)
			require.NoError(t, json.Unmarshal(data, &saved.Attributes))
			if tc.snapshot {
				require.Equal(t, []uuid.UUID{computeID}, saved.Attributes.ComponentsByType[devicetypes.ComponentTypeCompute])
				require.Equal(t, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}}, exec.lastRequest.Info.FirmwareRestartComponents)
			} else {
				require.Nil(t, saved.Attributes.FirmwareTargetsByType)
				require.Empty(t, exec.lastRequest.Info.FirmwareRestartComponents)
			}
			if tc.legacy {
				saved.Attributes.FirmwareTargetsByType = nil
				delete(saved.Attributes.ComponentsByType, devicetypes.ComponentTypeCompute)
			}
			if tc.missing {
				fullRack.Components = fullRack.Components[:1]
			}
			fullRack.AddComponent(newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "late-compute"))
			store.tasksByID = map[uuid.UUID]*taskdef.Task{saved.ID: &saved}
			err = m.promoteTask(context.Background(), saved.ID)
			if tc.missing {
				require.ErrorContains(t, err, "reserved component is missing")
				require.Equal(t, 1, exec.executeCalls)
				return
			}
			require.NoError(t, err)
			require.Len(t, exec.lastRequest.Info.Components, 1)
			if tc.snapshot && !tc.legacy {
				require.Equal(t, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}}, exec.lastRequest.Info.FirmwareRestartComponents)
			} else {
				require.Empty(t, exec.lastRequest.Info.FirmwareRestartComponents)
			}
		})
	}
}
