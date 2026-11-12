// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

func testNVOSRestartSubmission(t *testing.T) {
	for _, tc := range []struct {
		name           string
		targets        []string
		version        string
		snapshot       bool
		missing        bool
		unlinked       bool
		conflict       bool
		legacy         bool
		noFirmwareRule bool
		newlyRequired  bool
		changedRule    bool
	}{
		{name: "NVOS snapshots full compute scope", targets: []string{"nvos"}, snapshot: true},
		{name: "whole bundle includes NVOS", snapshot: true},
		{name: "BMC-only retains original scope", targets: []string{"bmc"}},
		{name: "omitted switch layer does not reserve compute", version: `{"compute":{"Id":"compute-fw"}}`},
		{name: "missing reserved compute fails recovery", targets: []string{"nvos"}, snapshot: true, missing: true},
		{name: "unaddressable compute rejects before persistence", targets: []string{"nvos"}, unlinked: true},
		{name: "compute power task blocks switch NVOS update", targets: []string{"nvos"}, conflict: true},
		{name: "predecessor task retains firmware-only behavior", targets: []string{"nvos"}, snapshot: true, legacy: true},
		{name: "custom rule without update ignores unaddressable compute", targets: []string{"nvos"}, noFirmwareRule: true, unlinked: true},
		{name: "changed rule no longer requires missing restart-only compute", targets: []string{"nvos"}, snapshot: true, changedRule: true, missing: true},
		{name: "new restart requirement rejects promotion without dispatch", targets: []string{"nvos"}, noFirmwareRule: true, newlyRequired: true},
		{name: "predecessor without decision retains behavior after rule change", targets: []string{"nvos"}, noFirmwareRule: true, newlyRequired: true, legacy: true},
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
			inv := &submitTaskInventory{rack: fullRack, components: map[uuid.UUID]*component.Component{switchID: &sw}}
			store := &managerTaskStore{}
			if tc.noFirmwareRule {
				store.operationRule = &operationrules.OperationRule{ID: uuid.New(),
					RuleDefinition: operationrules.RuleDefinition{Version: "v1", Steps: []operationrules.SequenceStep{{
						ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 1,
						MainOperation: operationrules.ActionConfig{Name: operationrules.ActionSleep,
							Parameters: map[string]any{operationrules.ParamDuration: "1s"}},
					}}}}
			}
			exec := &managerExecutor{executionID: "execution-1"}
			m := &ManagerImpl{inventoryStore: inv, taskStore: store, executor: exec,
				ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			raw, err := (&operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: tc.targets, TargetVersion: tc.version}).Marshal()
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
			if (tc.unlinked && !tc.noFirmwareRule) || tc.conflict {
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
			var predecessor struct {
				ComponentsByType map[devicetypes.ComponentType][]uuid.UUID `json:"components_by_type"`
			}
			require.NoError(t, json.Unmarshal(data, &predecessor))
			require.Empty(t, predecessor.ComponentsByType[devicetypes.ComponentTypeCompute], "old readers must not flash restart-only compute")
			if tc.snapshot {
				require.Equal(t, []uuid.UUID{computeID}, saved.Attributes.FirmwareRestartComputeIDs)
				require.Equal(t, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}}, exec.lastRequest.Info.FirmwareRestartComponents)
			} else {
				require.Nil(t, saved.Attributes.FirmwareRestartComputeIDs)
				require.Empty(t, exec.lastRequest.Info.FirmwareRestartComponents)
			}
			if tc.legacy {
				saved.Attributes.FirmwareRestartRequired = nil
				saved.Attributes.FirmwareRestartComputeIDs = nil
				delete(saved.Attributes.ComponentsByType, devicetypes.ComponentTypeCompute)
			}
			if tc.missing {
				fullRack.Components = fullRack.Components[:1]
			}
			fullRack.AddComponent(newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "late-compute"))
			if tc.changedRule {
				store.operationRule = &operationrules.OperationRule{ID: uuid.New(),
					RuleDefinition: operationrules.RuleDefinition{Version: "v1", Steps: []operationrules.SequenceStep{{
						ComponentType: devicetypes.ComponentTypeNVSwitch, Stage: 1,
						MainOperation: operationrules.ActionConfig{Name: operationrules.ActionSleep,
							Parameters: map[string]any{operationrules.ParamDuration: "1s"}},
					}}}}
			}
			if tc.newlyRequired {
				store.operationRule = nil // Resolve the firmware default on promotion.
			}
			store.tasksByID = map[uuid.UUID]*taskdef.Task{saved.ID: &saved}
			err = m.promoteTask(context.Background(), saved.ID)
			if tc.newlyRequired && !tc.legacy {
				require.ErrorContains(t, err, "restart requirements changed after admission")
				require.Equal(t, 1, exec.executeCalls, "promotion must not dispatch firmware")
				require.NotEmpty(t, store.statusUpdates)
				require.Equal(t, taskcommon.TaskStatusFailed, store.statusUpdates[len(store.statusUpdates)-1].Status)
				return
			}
			if tc.missing && !tc.changedRule {
				require.ErrorContains(t, err, "reserved component is missing")
				require.Equal(t, 1, exec.executeCalls)
				return
			}
			require.NoError(t, err)
			require.Len(t, exec.lastRequest.Info.Components, 1)
			if tc.snapshot && !tc.legacy && !tc.changedRule {
				require.Equal(t, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}}, exec.lastRequest.Info.FirmwareRestartComponents)
			} else {
				require.Empty(t, exec.lastRequest.Info.FirmwareRestartComponents)
			}
		})
	}
}
