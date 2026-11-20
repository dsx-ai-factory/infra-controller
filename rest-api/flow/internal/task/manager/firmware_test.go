// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

func testNVOSRestartSubmission(t *testing.T) {
	for _, tc := range []struct {
		name                 string
		targets              []string
		version              string
		snapshot             bool
		missing              bool
		unlinked             bool
		conflict             bool
		legacy               bool
		noFirmwareRule       bool
		newlyRequired        bool
		changedRule          bool
		growDuringValidation bool
		idempotent           bool
	}{
		{name: "NVOS snapshots full compute scope", targets: []string{"nvos"}, snapshot: true},
		{name: "creation reuses the preflight snapshot", targets: []string{"nvos"}, snapshot: true, growDuringValidation: true},
		{name: "idempotent creation reuses the preflight snapshot", targets: []string{"nvos"}, snapshot: true, growDuringValidation: true, idempotent: true},
		{name: "normalized NVOS selection reserves compute", targets: []string{" NVOS "}, snapshot: true},
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
			if tc.growDuringValidation {
				exec.onValidate = func(*taskdef.ExecutionRequest) error {
					fullRack.AddComponent(newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "added-after-preflight"))
					return nil
				}
			}
			m := &ManagerImpl{inventoryStore: inv, taskStore: store, executor: exec,
				ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			raw, err := (&operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: tc.targets, TargetVersion: tc.version}).Marshal()
			require.NoError(t, err)
			if tc.conflict {
				active := newTaskForRack(t.Context(), &operation.Request{Operation: testPowerControlOperation(t)}, fullRack)
				store.activeTasksByRack = map[uuid.UUID][]*taskdef.Task{rackID: {&active}}
			}
			req := &operation.Request{
				Operation:  operation.Wrapper{Type: taskcommon.TaskTypeFirmwareControl, Code: taskcommon.OpCodeFirmwareControlUpgrade, Info: raw},
				TargetSpec: operation.TargetSpec{Components: []operation.ComponentTarget{{UUID: switchID}}},
			}
			if tc.idempotent {
				req.RequiredRackID = rackID
				req.IdempotencyKey = "preflight-snapshot"
			}
			ids, err := m.SubmitTask(context.Background(), req)
			if tc.unlinked && !tc.noFirmwareRule {
				require.ErrorContains(t, err, computeID.String())
				require.Empty(t, ids)
				require.Zero(t, store.createTaskCalls)
				require.Zero(t, exec.executeCalls)
				return
			}
			require.NoError(t, err)
			if tc.conflict {
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
				assertRestartTargets(t, exec.lastValidatedRequest.Info, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}})
				require.Equal(t, []uuid.UUID{computeID}, saved.Attributes.FirmwareRestartComputeIDs)
				assertRestartTargets(t, exec.lastRequest.Info, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}})
			} else {
				require.Nil(t, saved.Attributes.FirmwareRestartComputeIDs)
				require.Empty(t, exec.lastRequest.Info.AdditionalSteps)
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
				assertRestartTargets(t, exec.lastRequest.Info, []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}})
			} else {
				require.Empty(t, exec.lastRequest.Info.AdditionalSteps)
			}
		})
	}
}

// nvosPreflightInventory distinguishes the selected switch subset from the full
// rack inventory used only for restart preparation.
type nvosPreflightInventory struct {
	submitTaskInventory
	racks         map[uuid.UUID]*rack.Rack
	restartErrors map[uuid.UUID]error
}

func (s *nvosPreflightInventory) GetRackByIdentifier(
	_ context.Context, id identifier.Identifier, withComponents bool,
) (*rack.Rack, error) {
	s.getRackCalls++
	if withComponents && s.restartErrors[id.ID] != nil {
		return nil, s.restartErrors[id.ID]
	}
	return s.racks[id.ID], nil
}

func TestManagerImpl_ValidateTask(t *testing.T) {
	testNVOSRestartPreflight(t, func(m *ManagerImpl, ctx context.Context, req *operation.Request) ([]uuid.UUID, error) {
		return nil, m.ValidateTask(ctx, req)
	})
}

func testNVOSRestartPreflight(t *testing.T, run func(*ManagerImpl, context.Context, *operation.Request) ([]uuid.UUID, error)) {
	for _, tc := range []struct {
		name           string
		unlinked       bool
		inventoryError bool
		preempt        bool
		acceptedKey    bool
		want           string
		wantCode       codes.Code
	}{
		{name: "firmware preemption is rejected before preparation", preempt: true, want: "firmware tasks do not support preemption", wantCode: codes.InvalidArgument},
		{name: "firmware preemption cannot bypass rejection with an accepted key", preempt: true, acceptedKey: true, want: "firmware tasks do not support preemption", wantCode: codes.InvalidArgument},
		{name: "unlinked restart compute rejects the entire submission", unlinked: true, want: "selected components not linked", wantCode: codes.FailedPrecondition},
		{name: "restart inventory failure is returned before any task", inventoryError: true, want: "restart inventory unavailable", wantCode: codes.Unknown},
		{name: "unsupported restart capability rejects before persistence", want: "compute power control not supported", wantCode: codes.FailedPrecondition},
	} {
		t.Run(tc.name, func(t *testing.T) {
			first := uuid.MustParse("00000000-0000-0000-0000-000000000001")
			second := uuid.MustParse("00000000-0000-0000-0000-000000000002")
			inv := &nvosPreflightInventory{
				submitTaskInventory: submitTaskInventory{components: make(map[uuid.UUID]*component.Component)},
				racks:               make(map[uuid.UUID]*rack.Rack),
				restartErrors:       make(map[uuid.UUID]error),
			}
			var selected []operation.ComponentTarget
			var badCompute uuid.UUID
			for _, id := range []uuid.UUID{first, second} {
				full := newTestRack(id, id.String())
				sw := newTestComponent(uuid.New(), id, devicetypes.ComponentTypeNVSwitch, "switch-"+id.String())
				coreID := "good-compute"
				if id == second {
					coreID = "bad-compute"
				}
				compute := newTestComponent(uuid.New(), id, devicetypes.ComponentTypeCompute, coreID)
				if id == second && tc.unlinked {
					compute.ComponentID = ""
					badCompute = compute.Info.ID
				}
				full.AddComponent(sw)
				full.AddComponent(compute)
				inv.racks[id] = full
				inv.components[sw.Info.ID] = &sw
				selected = append(selected, operation.ComponentTarget{UUID: sw.Info.ID})
			}
			if tc.inventoryError {
				inv.restartErrors[second] = errors.New("restart inventory unavailable")
			}
			store := &managerTaskStore{}
			exec := &managerExecutor{onValidate: func(req *taskdef.ExecutionRequest) error {
				if tc.unlinked || tc.inventoryError {
					return nil
				}
				for _, step := range req.Info.AdditionalSteps {
					for _, c := range step.Components {
						if c.ComponentID == "bad-compute" {
							return errors.New("compute power control not supported")
						}
					}
				}
				return nil
			}}
			m := &ManagerImpl{inventoryStore: inv, taskStore: store, executor: exec,
				ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			raw, err := (&operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: []string{"nvos"}}).Marshal()
			require.NoError(t, err)
			req := &operation.Request{
				Operation:  operation.Wrapper{Type: taskcommon.TaskTypeFirmwareControl, Code: taskcommon.OpCodeFirmwareControlUpgrade, Info: raw},
				TargetSpec: operation.TargetSpec{Components: selected},
			}
			if tc.preempt {
				req.ConflictStrategy = operation.ConflictStrategyPreempt
			}
			if tc.acceptedKey {
				req.RequiredRackID = second
				req.IdempotencyKey = "scheduled-firmware"
				store.taskByIdempotencyKey = map[string]*taskdef.Task{req.IdempotencyKey: {
					ID: uuid.New(), RackID: second, ExecutionID: "already-scheduled", IdempotencyKey: req.IdempotencyKey,
				}}
			}
			ids, err := run(m, t.Context(), req)
			require.ErrorContains(t, err, tc.want)
			require.Equal(t, tc.wantCode, status.Code(err))
			if tc.unlinked {
				require.ErrorContains(t, err, badCompute.String())
			}
			if tc.preempt {
				require.Zero(t, inv.getRackCalls)
				require.Zero(t, exec.validateCalls)
			} else {
				require.ErrorContains(t, err, second.String())
			}
			require.Empty(t, ids)
			require.Zero(t, store.createTaskCalls)
			require.Zero(t, exec.executeCalls)
		})
	}
}

func testPendingNVOSRetryScope(t *testing.T) {
	for _, tc := range []struct {
		name     string
		captured bool
	}{
		{name: "predecessor without a restart snapshot"},
		{name: "accepted task with a restart snapshot", captured: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rackID, switchID, computeID := uuid.New(), uuid.New(), uuid.New()
			full := newTestRack(rackID, "rack-1")
			sw := newTestComponent(switchID, rackID, devicetypes.ComponentTypeNVSwitch, "switch-1")
			compute := newTestComponent(computeID, rackID, devicetypes.ComponentTypeCompute, "compute-1")
			unlinked := newTestComponent(uuid.New(), rackID, devicetypes.ComponentTypeCompute, "")
			full.AddComponent(sw)
			full.AddComponent(compute)
			full.AddComponent(unlinked)
			raw, err := (&operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: []string{"nvos"}}).Marshal()
			require.NoError(t, err)
			req := &operation.Request{Operation: operation.Wrapper{Type: taskcommon.TaskTypeFirmwareControl, Code: taskcommon.OpCodeFirmwareControlUpgrade, Info: raw},
				RequiredRackID: rackID, IdempotencyKey: "accepted-nvos-task",
				TargetSpec: operation.TargetSpec{Components: []operation.ComponentTarget{{UUID: switchID}}}}
			selected := rack.New(full.Info, full.Loc)
			selected.AddComponent(sw)
			accepted := newTaskForRack(t.Context(), req, selected)
			accepted.Status = taskcommon.TaskStatusPending
			var expected []taskdef.WorkflowComponent
			if tc.captured {
				restartRequired := true
				accepted.Attributes.FirmwareRestartRequired = &restartRequired
				accepted.Attributes.FirmwareRestartComputeIDs = []uuid.UUID{computeID}
				expected = []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute-1"}}
			}
			store := &managerTaskStore{taskByIdempotencyKey: map[string]*taskdef.Task{req.IdempotencyKey: &accepted}}
			exec := &managerExecutor{executionID: "accepted-execution"}
			m := &ManagerImpl{inventoryStore: &submitTaskInventory{rack: full, components: map[uuid.UUID]*component.Component{switchID: &sw}},
				taskStore: store, executor: exec, ruleResolver: operationrules.NewResolver(store), conflictResolver: conflict.NewResolver(store)}
			ids, err := m.SubmitTask(t.Context(), req)
			require.NoError(t, err)
			require.Equal(t, []uuid.UUID{accepted.ID}, ids)
			require.Zero(t, store.createTaskCalls)
			require.Equal(t, 1, exec.executeCalls)
			assertRestartTargets(t, exec.lastRequest.Info, expected)
		})
	}
}

func assertRestartTargets(t *testing.T, info taskdef.ExecutionInfo, expected []taskdef.WorkflowComponent) {
	t.Helper()
	if len(expected) == 0 {
		require.Empty(t, info.AdditionalSteps)
		return
	}
	require.Len(t, info.AdditionalSteps, 2)
	for i, step := range info.AdditionalSteps {
		require.Equal(t, expected, step.Components)
		require.Equal(t, info.RuleDefinition.GetMaxStage()+i+1, step.Definition.Stage)
		require.Equal(t, operationrules.ActionPowerControl, step.Definition.MainOperation.Name)
	}
	require.Equal(t, "power_off", info.AdditionalSteps[0].Definition.MainOperation.Parameters[operationrules.ParamOperation])
	require.Equal(t, "power_on", info.AdditionalSteps[1].Definition.MainOperation.Parameters[operationrules.ParamOperation])
}
