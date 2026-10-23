// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/base32"
	"fmt"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// rackPowerInventory records the expanded rack lookup and permits a controlled
// delay or failure before the shared fake resolves the external ID.
type rackPowerInventory struct {
	*mockManager
	calls          int
	withComponents bool
	deadline       time.Time
	beforeRead     func(context.Context) error
	nilRack        bool
}

// GetRackByIdentifier delegates external-ID lookup to mockManager after the hook.
func (m *rackPowerInventory) GetRackByIdentifier(ctx context.Context, id identifier.Identifier, withComponents bool) (*rack.Rack, error) {
	m.calls++
	m.withComponents = withComponents
	m.deadline, _ = ctx.Deadline()
	if m.beforeRead != nil {
		err := m.beforeRead(ctx)
		if err != nil {
			return nil, err
		}
	}
	if m.nilRack {
		return nil, nil
	}
	return m.mockManager.GetRackByIdentifier(ctx, id, withComponents)
}

// rackPowerCoreReader records batch requests and returns healthy facts for each
// requested identity. Hooks control blocking reads and altered batch responses.
type rackPowerCoreReader struct {
	requests   []*corev1.GetComponentPowerDetailsRequest
	deadlines  []time.Time
	beforeRead func(context.Context, *corev1.GetComponentPowerDetailsRequest) error
	alter      func(*corev1.GetComponentPowerDetailsResponse) error
}

// GetComponentPowerDetails returns ordered facts for exactly the requested kind.
func (r *rackPowerCoreReader) GetComponentPowerDetails(ctx context.Context, request *corev1.GetComponentPowerDetailsRequest) (*corev1.GetComponentPowerDetailsResponse, error) {
	r.requests = append(r.requests, request)
	deadline, _ := ctx.Deadline()
	r.deadlines = append(r.deadlines, deadline)
	if r.beforeRead != nil {
		err := r.beforeRead(ctx, request)
		if err != nil {
			return nil, err
		}
	}
	var ids []string
	for _, id := range request.GetMachineIds().GetMachineIds() {
		ids = append(ids, id.Id)
	}
	for _, id := range request.GetSwitchIds().GetIds() {
		ids = append(ids, id.Id)
	}
	for _, id := range request.GetPowerShelfIds().GetIds() {
		ids = append(ids, id.Id)
	}
	response := &corev1.GetComponentPowerDetailsResponse{}
	for _, id := range ids {
		observedSource := corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_RMS
		policySource := corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_CORE_POWER_POLICY
		policy := &corev1.ComponentPowerPolicy{Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE}
		if request.GetMachineIds() != nil {
			state := corev1.PowerState_On
			policy = &corev1.ComponentPowerPolicy{Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE, State: &state, Source: &policySource}
		}
		response.Entries = append(response.Entries, &corev1.ComponentPowerDetails{
			ComponentId: id,
			Actual: &corev1.ComponentPowerObservation{
				Outcome: &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON},
				Source:  &observedSource, ObservedAt: timestamppb.New(time.Date(2026, 10, 15, 0, 0, 0, 0, time.UTC)),
			},
			Desired: policy,
			Actions: &corev1.ComponentPowerActionDetails{Actions: []*corev1.ComponentPowerAction{{Status: corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_ACTIVE, Operation: corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_ON}}},
		})
	}
	if r.alter != nil {
		return response, r.alter(response)
	}
	return response, nil
}

// rackPowerFixture keeps physical membership, managed identity and historical
// task selections separate, because only membership determines the tray list.
type rackPowerFixture struct {
	server       *FlowServerImpl
	inventory    *rackPowerInventory
	rack         *rack.Rack
	core         *rackPowerCoreReader
	store        *powerDetailsTaskStore
	tasks        []taskdef.PowerTaskDetails
	wantDeadline time.Time
}

// newRackPowerFixture creates deliberately unsorted compute, switch and shelf
// members; each has a different managed ID and an available passive read.
func newRackPowerFixture() *rackPowerFixture {
	r := &rack.Rack{Info: deviceinfo.DeviceInfo{ID: uuid.New()}, ExternalID: "rack-001"}
	r.Components = []component.Component{
		{Info: deviceinfo.DeviceInfo{ID: uuid.MustParse("00000000-0000-0000-0000-000000000001")},
			Type: devicetypes.ComponentTypePowerShelf, ComponentID: rackPowerCoreID("ps100rt", 1), RackID: r.Info.ID},
		{Info: deviceinfo.DeviceInfo{ID: uuid.MustParse("00000000-0000-0000-0000-000000000003")},
			Type: devicetypes.ComponentTypeCompute, ComponentID: rackPowerCoreID("fm100ht", 2), RackID: r.Info.ID},
		{Info: deviceinfo.DeviceInfo{ID: uuid.MustParse("00000000-0000-0000-0000-000000000004")},
			Type: devicetypes.ComponentTypeNVSwitch, ComponentID: rackPowerCoreID("sw100nt", 3), RackID: r.Info.ID},
	}
	inventory := &rackPowerInventory{mockManager: newMockManager()}
	inventory.racks[r.Info.ID] = r
	core := &rackPowerCoreReader{}
	store := &powerDetailsTaskStore{}
	f := &rackPowerFixture{
		server:    &FlowServerImpl{inventoryManager: inventory, taskStore: store, powerReader: core},
		inventory: inventory, rack: r, core: core, store: store,
	}
	store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) { return f.tasks, nil }
	return f
}

// rackPowerCoreID encodes a distinct 32-byte hardware hash under a typed prefix.
func rackPowerCoreID(prefix string, index byte) string {
	var hash [32]byte
	hash[0], hash[31] = index, 255
	return prefix + strings.ToLower(base32.HexEncoding.WithPadding(base32.NoPadding).EncodeToString(hash[:]))
}

// rackPowerTask supplies valid stored intent while keeping its selections visible.
func rackPowerTask(rackID uuid.UUID, targets operation.ComponentsByType) taskdef.PowerTaskDetails {
	created := time.Date(2026, 10, 15, 0, 0, 0, 0, time.UTC)
	return taskdef.PowerTaskDetails{
		ID: uuid.New(), RackID: rackID, Status: taskcommon.TaskStatusWaiting,
		Operation:  operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
		Attributes: taskcommon.TaskAttributes{ComponentsByType: targets},
		CreatedAt:  created, UpdatedAt: created,
	}
}

// TestFlowServerImpl_GetRackPowerDetails proves the rack aggregation boundaries:
// membership expansion, batch isolation, compiled operations and transport size.
// The retained tray tests cover the shared per-entry conversion matrix.
func TestFlowServerImpl_GetRackPowerDetails(t *testing.T) {
	tests := []struct {
		name           string
		transport      bool
		controlledTime bool
		configure      func(*rackPowerFixture)
		invoke         func(*testing.T, *rackPowerFixture, *pb.GetRackPowerDetailsRequest) (*pb.GetRackPowerDetailsResponse, error)
		wantCode       codes.Code
		check          func(*testing.T, *rackPowerFixture, *pb.GetRackPowerDetailsResponse)
	}{
		{
			// Distinct facts for two compute members catch index reuse within a
			// batch. Inventory puts their Flow UUIDs in reverse output order.
			name: "complete members and unique tasks", transport: true,
			configure: func(f *rackPowerFixture) {
				f.rack.Components = append(f.rack.Components,
					component.Component{Info: deviceinfo.DeviceInfo{ID: uuid.MustParse("00000000-0000-0000-0000-000000000002")}, Type: devicetypes.ComponentTypeCompute, ComponentID: rackPowerCoreID("fm100ht", 4), RackID: f.rack.Info.ID},
					component.Component{Info: deviceinfo.DeviceInfo{ID: uuid.MustParse("00000000-0000-0000-0000-000000000005")}, Type: devicetypes.ComponentTypeCompute, RackID: f.rack.Info.ID},
				)
				f.core.alter = func(response *corev1.GetComponentPowerDetailsResponse) error {
					for _, entry := range response.Entries {
						if entry.ComponentId == f.rack.Components[3].ComponentID {
							entry.Actual.Outcome = &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_OFF}
							state := corev1.PowerState_Off
							entry.Desired.State = &state
							entry.Actions.Actions[0].Operation = corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_OFF
						}
					}
					return nil
				}
				task := rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{
					devicetypes.ComponentTypePowerShelf: {f.rack.Components[0].Info.ID},
					devicetypes.ComponentTypeCompute:    {f.rack.Components[1].Info.ID, uuid.MustParse("00000000-0000-0000-0000-000000000099")},
				})
				f.tasks = []taskdef.PowerTaskDetails{task, task}
				// One metadata read must survive rack deduplication and serve all
				// affected members without repeating context in their references.
				f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) {
					return []taskdef.TaskContextMetadata{
						// Shelf power conflicts with running firmware elsewhere in the rack.
						{ID: uuid.UUID{15: 0xe1}, RackID: f.rack.Info.ID, Type: taskcommon.TaskTypeFirmwareControl,
							Status: taskcommon.TaskStatusRunning, CreatedAt: task.CreatedAt.Add(-2 * time.Minute),
							Attributes: taskcommon.TaskAttributes{ComponentsByType: operation.ComponentsByType{
								devicetypes.ComponentTypeCompute: {f.rack.Components[1].Info.ID},
							}}},
						// Earlier bring-up is nearby waiting work, not an active conflict.
						{ID: uuid.UUID{15: 0xe2}, RackID: f.rack.Info.ID, Type: taskcommon.TaskTypeBringUp,
							Status: taskcommon.TaskStatusWaiting, CreatedAt: task.CreatedAt.Add(-time.Minute)},
					}, nil
				}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				d := response.Details
				require.Equal(t, f.rack.Info.ID.String(), d.FlowRackId.Id)
				require.Equal(t, f.rack.ExternalID, d.ExternalRackId)
				require.Len(t, f.core.requests, 3)
				require.Len(t, f.core.requests[0].GetMachineIds().GetMachineIds(), 2)
				require.Equal(t, f.rack.Components[3].ComponentID, f.core.requests[0].GetMachineIds().GetMachineIds()[0].Id)
				require.Equal(t, f.rack.Components[1].ComponentID, f.core.requests[0].GetMachineIds().GetMachineIds()[1].Id)
				require.Equal(t, f.rack.Components[2].ComponentID, f.core.requests[1].GetSwitchIds().GetIds()[0].Id)
				require.Equal(t, f.rack.Components[0].ComponentID, f.core.requests[2].GetPowerShelfIds().GetIds()[0].Id)
				require.Len(t, d.Trays, 5)
				require.Len(t, d.Operations.Actions, 5)
				require.Len(t, d.Operations.Tasks.Entries, 1)
				require.Len(t, d.Operations.Tasks.Entries[0].Targets.Groups, 2)
				require.Len(t, d.Operations.Tasks.Entries[0].Targets.Groups[0].ComponentIds, 2)
				require.Equal(t, f.tasks[0].Attributes.ComponentsByType[devicetypes.ComponentTypeCompute][1].String(), d.Operations.Tasks.Entries[0].Targets.Groups[0].ComponentIds[1].Id)
				require.Equal(t, pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_UNSPECIFIED, d.Operations.Tasks.Entries[0].Relevance)
				require.Equal(t, 1, f.store.calls)
				require.Equal(t, 1, f.store.contextCalls)
				require.Equal(t, f.rack.Info.ID, f.store.contextRackID)
				require.Equal(t, f.store.deadline, f.store.contextDeadline)
				wantContext := &pb.PowerTaskWaitingContext{
					ActiveConflicts: []*pb.TaskBrief{{Id: &pb.UUID{Id: (uuid.UUID{15: 0xe1}).String()},
						Kind: pb.TaskKind_TASK_KIND_FIRMWARE_CONTROL, Status: pb.TaskStatus_TASK_STATUS_RUNNING}},
					EarlierWaiting: []*pb.TaskBrief{{Id: &pb.UUID{Id: (uuid.UUID{15: 0xe2}).String()},
						Kind: pb.TaskKind_TASK_KIND_BRING_UP, Status: pb.TaskStatus_TASK_STATUS_WAITING}},
				}
				require.True(t, proto.Equal(wantContext, d.Operations.Tasks.Entries[0].WaitingContext))
				for i, expected := range []struct {
					memberIndex int
					kind        pb.ComponentType
				}{
					{3, pb.ComponentType_COMPONENT_TYPE_COMPUTE},
					{1, pb.ComponentType_COMPONENT_TYPE_COMPUTE},
					{4, pb.ComponentType_COMPONENT_TYPE_COMPUTE},
					{2, pb.ComponentType_COMPONENT_TYPE_NVSWITCH},
					{0, pb.ComponentType_COMPONENT_TYPE_POWERSHELF},
				} {
					memberIndex := expected.memberIndex
					member, tray, actions := f.rack.Components[memberIndex], d.Trays[i], d.Operations.Actions[i]
					require.Equal(t, member.Info.ID.String(), tray.FlowComponentId.Id)
					require.Equal(t, member.ComponentID, tray.GetCoreComponentId())
					require.Equal(t, member.Info.ID.String(), actions.FlowComponentId.Id)
					require.Equal(t, member.ComponentID, actions.GetCoreComponentId())
					require.Equal(t, expected.kind, tray.ComponentType)
					require.Equal(t, expected.kind, actions.ComponentType)
					require.True(t, proto.Equal(tray.Operations.Actions, actions.Details))
					require.Len(t, tray.Operations.Tasks.Entries, 1)
					require.Equal(t, f.tasks[0].ID.String(), tray.Operations.Tasks.Entries[0].Id.Id)
					wantRelevance := pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED
					if memberIndex == 0 || memberIndex == 1 {
						wantRelevance = pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED
					}
					require.Equal(t, wantRelevance, tray.Operations.Tasks.Entries[0].Relevance)
				}
				for i, expected := range []struct {
					actual  corev1.ComponentPowerState
					desired corev1.PowerState
					action  corev1.ComponentPowerActionOperation
				}{
					{corev1.ComponentPowerState_COMPONENT_POWER_STATE_OFF, corev1.PowerState_Off, corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_OFF},
					{corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, corev1.PowerState_On, corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_ON},
				} {
					require.Equal(t, expected.actual, d.Trays[i].Actual.GetValue().GetState())
					require.NotNil(t, d.Trays[i].Desired.GetValue().State)
					require.Equal(t, expected.desired, d.Trays[i].Desired.GetValue().GetState())
					require.Equal(t, expected.action, d.Trays[i].Operations.Actions.GetValue().Actions[0].Operation)
				}
				requireRackCoreError(t, d.Trays[2], pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED)
				require.Nil(t, d.Trays[2].CoreComponentId)
				require.Nil(t, d.Operations.Actions[2].CoreComponentId)
				require.Equal(t, pb.RackPowerState_RACK_POWER_STATE_MIXED, d.Actual.State)
				require.Equal(t, pb.RackPowerState_RACK_POWER_STATE_MIXED, d.Desired.State)
				require.Equal(t, uint32(4), d.Actual.Coverage.Known)
				require.Equal(t, uint32(1), d.Actual.Coverage.Unavailable)
				require.Equal(t, uint32(2), d.Desired.Coverage.Known)
				require.Equal(t, uint32(2), d.Desired.Coverage.NotApplicable)
				require.Equal(t, uint32(1), d.Desired.Coverage.Unavailable)
			},
		},
		{
			// A rack retains historical and untargeted tasks, while each member
			// references only its own selected work or a shelf's rack effect.
			name: "member task references exclude unrelated rack work",
			configure: func(f *rackPowerFixture) {
				f.tasks = []taskdef.PowerTaskDetails{
					rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{devicetypes.ComponentTypeCompute: {f.rack.Components[1].Info.ID}}),
					rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{devicetypes.ComponentTypeCompute: {uuid.MustParse("00000000-0000-0000-0000-000000000099")}}),
					rackPowerTask(f.rack.Info.ID, nil),
					rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{devicetypes.ComponentTypeCompute: {f.rack.Components[1].Info.ID}}),
				}
				for i := range f.tasks {
					f.tasks[i].CreatedAt = f.tasks[i].CreatedAt.Add(time.Duration(i) * time.Minute)
					f.tasks[i].UpdatedAt = f.tasks[i].CreatedAt
				}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				d := response.Details
				require.Len(t, d.Operations.Tasks.Entries, len(f.tasks))
				for i, task := range f.tasks {
					require.Equal(t, task.ID.String(), d.Operations.Tasks.Entries[i].Id.Id)
				}
				require.Equal(t, f.tasks[1].Attributes.ComponentsByType[devicetypes.ComponentTypeCompute][0].String(), d.Operations.Tasks.Entries[1].Targets.Groups[0].ComponentIds[0].Id)
				require.Nil(t, d.Operations.Tasks.Entries[2].Targets)
				require.Len(t, d.Trays[0].Operations.Tasks.Entries, 2)
				for i, taskIndex := range []int{0, 3} {
					reference := d.Trays[0].Operations.Tasks.Entries[i]
					require.Equal(t, f.tasks[taskIndex].ID.String(), reference.Id.Id)
					require.Equal(t, pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED, reference.Relevance)
				}
				for _, tray := range d.Trays[1:] {
					require.NotNil(t, tray.Operations.Tasks)
					require.Nil(t, tray.Operations.Tasks.Error)
					require.Empty(t, tray.Operations.Tasks.Entries)
				}
			},
		},
		{
			// Core's fixed maximum is 64, independent of any production constant
			// used by the chunking loop. A malformed first batch cannot erase #65.
			name: "64 target batches preserve a healthy later batch",
			configure: func(f *rackPowerFixture) {
				f.rack.Components = nil
				for i := range 65 {
					f.rack.Components = append(f.rack.Components, component.Component{Info: deviceinfo.DeviceInfo{ID: uuid.MustParse(fmt.Sprintf("00000000-0000-0000-0000-%012d", i+1))}, Type: devicetypes.ComponentTypeCompute, ComponentID: rackPowerCoreID("fm100ht", byte(i)), RackID: f.rack.Info.ID})
				}
				f.core.alter = func(response *corev1.GetComponentPowerDetailsResponse) error {
					if len(response.Entries) == 64 {
						response.Entries[0].ComponentId = "wrong-owner"
					}
					return nil
				}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Len(t, f.core.requests, 2)
				require.Len(t, f.core.requests[0].GetMachineIds().GetMachineIds(), 64)
				require.Len(t, f.core.requests[1].GetMachineIds().GetMachineIds(), 1)
				require.Len(t, response.Details.Trays, 65)
				require.Equal(t, uint32(64), response.Details.Actual.Coverage.Unavailable)
				require.Equal(t, uint32(1), response.Details.Actual.Coverage.Known)
				for _, tray := range response.Details.Trays[:64] {
					requireRackCoreError(t, tray, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE)
				}
				require.Equal(t, rackPowerCoreID("fm100ht", 64), response.Details.Trays[64].GetCoreComponentId())
				require.NotNil(t, response.Details.Trays[64].Actual.GetValue())
			},
		},
		{
			// Correct identities cannot make a truncated batch valid; a missing
			// compute result must not prevent later switch and shelf reads.
			name: "missing batch entry preserves healthy later batches",
			configure: func(f *rackPowerFixture) {
				f.core.alter = func(response *corev1.GetComponentPowerDetailsResponse) error {
					if response.Entries[0].ComponentId == f.rack.Components[1].ComponentID {
						response.Entries = nil
					}
					return nil
				}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Len(t, f.core.requests, 3)
				require.Len(t, response.Details.Trays, 3)
				requireRackCoreError(t, response.Details.Trays[0], pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE)
				for _, tray := range response.Details.Trays[1:] {
					require.Equal(t, corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, tray.Actual.GetValue().GetState())
				}
				require.Equal(t, uint32(2), response.Details.Actual.Coverage.Known)
				require.Equal(t, uint32(1), response.Details.Actual.Coverage.Unavailable)
			},
		},
		{
			name: "Core RPC failure preserves healthy later batches and tasks",
			configure: func(f *rackPowerFixture) {
				f.tasks = []taskdef.PowerTaskDetails{rackPowerTask(f.rack.Info.ID, nil)}
				f.core.alter = func(response *corev1.GetComponentPowerDetailsResponse) error {
					if response.Entries[0].ComponentId == f.rack.Components[1].ComponentID {
						return status.Error(codes.Unavailable, "private dependency text")
					}
					return nil
				}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Len(t, f.core.requests, 3)
				requireRackCoreError(t, response.Details.Trays[0], pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED)
				for _, tray := range response.Details.Trays[1:] {
					require.Equal(t, corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, tray.Actual.GetValue().GetState())
					require.Equal(t, corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE, tray.Desired.GetValue().GetStatus())
					require.Len(t, tray.Operations.Actions.GetValue().Actions, 1)
				}
				require.Equal(t, uint32(2), response.Details.Actual.Coverage.Known)
				require.Equal(t, uint32(1), response.Details.Actual.Coverage.Unavailable)
				require.Len(t, response.Details.Operations.Tasks.Entries, 1)
				require.Equal(t, f.tasks[0].ID.String(), response.Details.Operations.Tasks.Entries[0].Id.Id)
				require.NotContains(t, response.String(), "private dependency text")
			},
		},
		{
			// Inventory accepts arbitrary Core links. DPU and malformed IDs
			// must not reach Core's HostMachineId decoder with a healthy host.
			name: "invalid Core links cannot poison a healthy host batch",
			configure: func(f *rackPowerFixture) {
				for i := range f.rack.Components {
					f.rack.Components[i].Type = devicetypes.ComponentTypeCompute
				}
				f.rack.Components[0].ComponentID = rackPowerCoreID("fm100ds", 1)
				f.rack.Components[2].ComponentID = "malformed-core-id"
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Len(t, f.core.requests, 1)
				require.Len(t, f.core.requests[0].GetMachineIds().GetMachineIds(), 1)
				require.Equal(t, f.rack.Components[1].ComponentID, f.core.requests[0].GetMachineIds().GetMachineIds()[0].Id)
				requireRackCoreError(t, response.Details.Trays[0], pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED)
				requireRackCoreError(t, response.Details.Trays[2], pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED)
				require.Equal(t, corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, response.Details.Trays[1].Actual.GetValue().GetState())
				require.Equal(t, corev1.PowerState_On, response.Details.Trays[1].Desired.GetValue().GetState())
				require.Len(t, response.Details.Trays[1].Operations.Actions.GetValue().Actions, 1)
				require.Equal(t, uint32(1), response.Details.Actual.Coverage.Known)
				require.Equal(t, uint32(2), response.Details.Actual.Coverage.Unavailable)
			},
		},
		{
			// AddComponent preserves supplied Core IDs and SQL uniqueness is
			// case-sensitive. Core decodes these two valid strings to one host,
			// so the rack read must avoid a duplicate typed-ID request.
			name: "Core hash aliases share one read",
			configure: func(f *rackPowerFixture) {
				f.rack.Components = f.rack.Components[:2]
				for i := range f.rack.Components {
					f.rack.Components[i].Type = devicetypes.ComponentTypeCompute
					f.rack.Components[i].ComponentID = powerDetailsCanonicalMachineID
				}
				f.rack.Components[0].ComponentID = powerDetailsCanonicalMachineID[:7] + strings.ToUpper(powerDetailsCanonicalMachineID[7:])
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Len(t, f.core.requests, 1)
				require.Len(t, f.core.requests[0].GetMachineIds().GetMachineIds(), 1)
				require.Equal(t, powerDetailsCanonicalMachineID, f.core.requests[0].GetMachineIds().GetMachineIds()[0].Id)
				require.Len(t, response.Details.Trays, 2)
				for i, tray := range response.Details.Trays {
					require.Equal(t, f.rack.Components[i].ComponentID, tray.GetCoreComponentId())
					require.Equal(t, f.rack.Components[i].Info.ID.String(), tray.FlowComponentId.Id)
					require.NotNil(t, tray.Actual.GetValue())
				}
				require.Equal(t, uint32(2), response.Details.Actual.Coverage.Known)
			},
		},
		{
			// A task-store failure must retain all Core facts and must not become
			// an apparently successful empty task list on any member.
			name: "failed tasks retain Core facts",
			configure: func(f *rackPowerFixture) {
				f.store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) {
					return nil, status.Error(codes.Unavailable, "private dependency text")
				}
			},
			check: func(t *testing.T, _ *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Equal(t, pb.RackPowerState_RACK_POWER_STATE_ON, response.Details.Actual.State)
				require.NotNil(t, response.Details.Operations.Tasks.Error)
				for _, tray := range response.Details.Trays {
					require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED, tray.Operations.Tasks.GetError())
				}
				require.NotContains(t, response.String(), "private dependency text")
			},
		},
		{
			// An absent provider must not erase inventory membership or a
			// successful empty task read.
			name:      "unconfigured Core",
			configure: func(f *rackPowerFixture) { f.server.powerReader = nil },
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Empty(t, f.core.requests)
				require.Nil(t, response.Details.Operations.Tasks.Error)
				require.Equal(t, uint32(3), response.Details.Actual.Coverage.Unavailable)
				for _, tray := range response.Details.Trays {
					requireRackCoreError(t, tray, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED)
				}
			},
		},
		{
			// Empty membership is a valid inventory result, never vacuous On.
			name: "empty rack retains historical work",
			configure: func(f *rackPowerFixture) {
				f.tasks = []taskdef.PowerTaskDetails{rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {f.rack.Components[1].Info.ID},
				})}
				f.rack.Components = nil
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Empty(t, f.core.requests)
				require.Empty(t, response.Details.Trays)
				require.Equal(t, pb.RackPowerState_RACK_POWER_STATE_UNKNOWN, response.Details.Actual.State)
				require.Equal(t, uint32(0), response.Details.Desired.Coverage.Total)
				require.Len(t, response.Details.Operations.Tasks.Entries, 1)
				require.Equal(t, f.tasks[0].ID.String(), response.Details.Operations.Tasks.Entries[0].Id.Id)
				require.Equal(t, f.tasks[0].Attributes.ComponentsByType[devicetypes.ComponentTypeCompute][0].String(), response.Details.Operations.Tasks.Entries[0].Targets.Groups[0].ComponentIds[0].Id)
			},
		},
		{
			// A shelf task affects the rack, but cannot turn an unsupported
			// inventory member into a supported task projection.
			name: "unsupported inventory member",
			configure: func(f *rackPowerFixture) {
				f.rack.Components = f.rack.Components[:1]
				f.rack.Components[0].Type = devicetypes.ComponentTypeCDU
				f.tasks = []taskdef.PowerTaskDetails{rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{
					devicetypes.ComponentTypePowerShelf: {uuid.MustParse("00000000-0000-0000-0000-000000000099")},
				})}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Empty(t, f.core.requests)
				require.Len(t, response.Details.Trays, 1)
				require.Len(t, response.Details.Operations.Tasks.Entries, 1)
				require.Equal(t, f.tasks[0].ID.String(), response.Details.Operations.Tasks.Entries[0].Id.Id)
				require.Equal(t, uint32(1), response.Details.Actual.Coverage.Unavailable)
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED, response.Details.Trays[0].Operations.Tasks.GetError())
				require.Empty(t, response.Details.Trays[0].Operations.Tasks.Entries)
				requireRackCoreError(t, response.Details.Trays[0], pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED)
			},
		},
		{
			// Inventory spends seven seconds of the single read budget. The
			// first batch completes, the second waits for expiry, and the final
			// kind must receive deadline errors without starting another call.
			name: "shared deadline preserves completed batches and tasks", controlledTime: true,
			configure: func(f *rackPowerFixture) {
				f.wantDeadline = time.Now().Add(30 * time.Second)
				f.inventory.beforeRead = func(context.Context) error {
					time.Sleep(7 * time.Second)
					return nil
				}
				f.tasks = []taskdef.PowerTaskDetails{rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: {f.rack.Components[1].Info.ID},
				})}
				f.core.beforeRead = func(ctx context.Context, request *corev1.GetComponentPowerDetailsRequest) error {
					if request.GetSwitchIds() != nil {
						<-ctx.Done()
						return ctx.Err()
					}
					return nil
				}
			},
			check: func(t *testing.T, f *rackPowerFixture, response *pb.GetRackPowerDetailsResponse) {
				require.Equal(t, f.wantDeadline, time.Now())
				require.Equal(t, f.wantDeadline, f.inventory.deadline)
				require.Equal(t, f.wantDeadline, f.store.deadline)
				require.Len(t, f.core.requests, 2)
				require.NotNil(t, f.core.requests[0].GetMachineIds())
				require.NotNil(t, f.core.requests[1].GetSwitchIds())
				require.Equal(t, []time.Time{f.wantDeadline, f.wantDeadline}, f.core.deadlines)
				d := response.Details
				require.Equal(t, corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, d.Trays[0].Actual.GetValue().GetState())
				require.Equal(t, corev1.PowerState_On, d.Trays[0].Desired.GetValue().GetState())
				require.Len(t, d.Trays[0].Operations.Actions.GetValue().Actions, 1)
				for _, tray := range d.Trays[1:] {
					requireRackCoreError(t, tray, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED)
				}
				require.Equal(t, pb.RackPowerState_RACK_POWER_STATE_UNKNOWN, d.Actual.State)
				require.Equal(t, pb.RackPowerState_RACK_POWER_STATE_UNKNOWN, d.Desired.State)
				require.True(t, proto.Equal(&pb.RackPowerCoverage{Total: 3, Known: 1, Unavailable: 2}, d.Actual.Coverage))
				require.True(t, proto.Equal(&pb.RackPowerCoverage{Total: 3, Known: 1, Unavailable: 2}, d.Desired.Coverage))
				require.Nil(t, d.Operations.Tasks.Error)
				require.Len(t, d.Operations.Tasks.Entries, 1)
				require.Equal(t, f.tasks[0].ID.String(), d.Operations.Tasks.Entries[0].Id.Id)
				require.Len(t, d.Trays[0].Operations.Tasks.Entries, 1)
				require.Equal(t, f.tasks[0].ID.String(), d.Trays[0].Operations.Tasks.Entries[0].Id.Id)
				require.Equal(t, pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED, d.Trays[0].Operations.Tasks.Entries[0].Relevance)
			},
		},
		{
			name: "caller cancellation joins both readers", controlledTime: true, wantCode: codes.Canceled,
			invoke: func(t *testing.T, f *rackPowerFixture, request *pb.GetRackPowerDetailsRequest) (*pb.GetRackPowerDetailsResponse, error) {
				ctx, cancel := context.WithCancel(t.Context())
				started, canceled, completed := make(chan struct{}, 2), make(chan struct{}, 2), make(chan struct{}, 2)
				releaseCore, releaseTasks := make(chan struct{}, 1), make(chan struct{}, 1)
				defer func() {
					cancel()
					close(releaseCore)
					close(releaseTasks)
				}()
				f.core.beforeRead = func(ctx context.Context, _ *corev1.GetComponentPowerDetailsRequest) error {
					started <- struct{}{}
					<-ctx.Done()
					canceled <- struct{}{}
					<-releaseCore
					completed <- struct{}{}
					return ctx.Err()
				}
				f.store.read = func(ctx context.Context) ([]taskdef.PowerTaskDetails, error) {
					started <- struct{}{}
					<-ctx.Done()
					canceled <- struct{}{}
					<-releaseTasks
					completed <- struct{}{}
					return nil, ctx.Err()
				}
				type result struct {
					response *pb.GetRackPowerDetailsResponse
					err      error
				}
				returned := make(chan result, 1)
				go func() {
					response, err := f.server.GetRackPowerDetails(ctx, request)
					returned <- result{response, err}
				}()

				synctest.Wait()
				require.Len(t, started, 2)
				cancel()
				synctest.Wait()
				require.Len(t, canceled, 2)
				require.Empty(t, returned)
				releaseCore <- struct{}{}
				synctest.Wait()
				require.Len(t, completed, 1)
				require.Empty(t, returned, "the task reader still owns unfinished cleanup")
				releaseTasks <- struct{}{}
				got := <-returned
				require.Len(t, completed, 2)
				require.Len(t, f.core.requests, 1, "cancellation must prevent later batch calls")
				return got.response, got.err
			},
		},
		{
			// Rack tasks are complete or the RPC fails; the tray RPC's policy of
			// replacing oversized tasks with LIMIT_EXCEEDED does not apply here.
			name: "task section alone exceeds allowance", wantCode: codes.ResourceExhausted,
			configure: func(f *rackPowerFixture) {
				targets := make([]uuid.UUID, 45_000)
				for i := range targets {
					targets[i] = uuid.MustParse(fmt.Sprintf("00000000-0000-0000-0000-%012x", i+1))
				}
				f.tasks = []taskdef.PowerTaskDetails{rackPowerTask(f.rack.Info.ID, operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: targets,
				})}
			},
		},
		{
			// A complete rack response cannot silently discard actions to fit
			// Temporal's payload ceiling; HTML escaping is part of that size.
			name: "encoded response exceeds allowance", wantCode: codes.ResourceExhausted,
			configure: func(f *rackPowerFixture) {
				f.core.alter = func(response *corev1.GetComponentPowerDetailsResponse) error {
					initiator := strings.Repeat("<", 200_000)
					response.Entries[0].Actions.Actions[0].Initiator = &initiator
					return nil
				}
			},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			run := func(t *testing.T) {
				f := newRackPowerFixture()
				if test.configure != nil {
					test.configure(f)
				}

				var response *pb.GetRackPowerDetailsResponse
				var err error
				request := &pb.GetRackPowerDetailsRequest{Id: f.rack.ExternalID}
				if test.invoke != nil {
					response, err = test.invoke(t, f, request)
				} else if test.transport {
					response, err = powerDetailsRPCClient(t, f.server).GetRackPowerDetails(t.Context(), request)
				} else {
					response, err = f.server.GetRackPowerDetails(t.Context(), request)
				}

				require.Equal(t, test.wantCode, status.Code(err))
				if test.wantCode != codes.OK {
					require.Nil(t, response)
				} else {
					require.NotNil(t, response)
				}
				require.Equal(t, 1, f.inventory.calls)
				require.True(t, f.inventory.withComponents)
				require.Equal(t, 1, f.store.calls)
				require.Equal(t, f.rack.Info.ID, f.store.rackID)
				if test.check != nil {
					test.check(t, f, response)
				}
			}
			if test.controlledTime {
				synctest.Test(t, run)
			} else {
				run(t)
			}
		})
	}

	for _, test := range []struct {
		name, id string
		want     codes.Code
		lookups  int
		failure  codes.Code
		nilRack  bool
	}{
		// No selector must fail before either dependency can start.
		{name: "missing rack selector", want: codes.InvalidArgument},
		// Unknown external IDs do not become an empty rack.
		{name: "unknown rack selector", id: "absent-rack", want: codes.NotFound, lookups: 1},
		{name: "inventory internal failure", id: "rack-001", want: codes.Internal, lookups: 1, failure: codes.Internal},
		{name: "inventory read failure", id: "rack-001", want: codes.Unavailable, lookups: 1, failure: codes.Unknown},
		{name: "inventory returned no rack", id: "rack-001", want: codes.Internal, lookups: 1, nilRack: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			f := newRackPowerFixture()
			f.inventory.nilRack = test.nilRack
			if test.failure != codes.OK {
				f.inventory.beforeRead = func(context.Context) error {
					return status.Error(test.failure, "private inventory text")
				}
			}

			response, err := f.server.GetRackPowerDetails(t.Context(), &pb.GetRackPowerDetailsRequest{Id: test.id})

			require.Nil(t, response)
			require.Equal(t, test.want, status.Code(err))
			require.Equal(t, test.lookups, f.inventory.calls)
			require.Zero(t, f.store.calls)
			require.Empty(t, f.core.requests)
			require.NotContains(t, err.Error(), "private inventory text")
		})
	}
}

// requireRackCoreError checks the entire unavailable Core projection, including
// actions, so one failed section cannot hide a fabricated successful sibling.
func requireRackCoreError(t *testing.T, tray *pb.RackTrayPowerDetails, code pb.PowerDetailsReadError) {
	t.Helper()
	require.Equal(t, code, tray.Actual.GetError())
	require.Equal(t, code, tray.Desired.GetError())
	require.Equal(t, code, tray.Operations.Actions.GetError())
}

// TestNormalizeCorePowerTarget covers typed request constraints beyond the
// ordinary host, switch, shelf and case aliases exercised by the public RPC.
func TestNormalizeCorePowerTarget(t *testing.T) {
	host := rackPowerCoreID("fm100ht", 1)
	predicted := rackPowerCoreID("fm100ps", 1)
	hostShelf := rackPowerCoreID("ps100hs", 1)
	serialSwitch := rackPowerCoreID("sw100ns", 1)
	tests := []struct {
		name     string
		kind     devicetypes.ComponentType
		id, want string
	}{
		{name: "predicted host with serial hash", kind: devicetypes.ComponentTypeCompute, id: predicted, want: predicted},
		{name: "host power shelf with serial hash", kind: devicetypes.ComponentTypePowerShelf, id: hostShelf, want: hostShelf},
		{name: "switch with serial hash", kind: devicetypes.ComponentTypeNVSwitch, id: serialSwitch, want: serialSwitch},
		{name: "ID belongs to another component kind", kind: devicetypes.ComponentTypeCompute, id: serialSwitch},
		{name: "unsupported ID version", kind: devicetypes.ComponentTypeCompute, id: "fm200" + host[5:]},
		{name: "unsupported hash source", kind: devicetypes.ComponentTypeCompute, id: "fm100hx" + host[7:]},
		{name: "invalid hash alphabet", kind: devicetypes.ComponentTypeCompute, id: host[:7] + "w" + host[8:]},
		{name: "nonzero unused hash bits", kind: devicetypes.ComponentTypeCompute, id: host[:58] + "h"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, valid := normalizeCorePowerTarget(test.kind, test.id)
			require.Equal(t, test.want != "", valid)
			require.Equal(t, test.want, got)
		})
	}
}

// TestSummarizeRackPower separates disagreement from missing evidence and policy
// applicability. Coverage assertions prevent a plausible summary from hiding
// an unobserved member, a missing Core target or disabled management.
func TestSummarizeRackPower(t *testing.T) {
	knownOn := rackPowerSummaryMember(corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, corev1.PowerState_On)
	knownOff := rackPowerSummaryMember(corev1.ComponentPowerState_COMPONENT_POWER_STATE_OFF, corev1.PowerState_Off)
	unknownDisabled := rackPowerSummaryMember(corev1.ComponentPowerState_COMPONENT_POWER_STATE_UNKNOWN, corev1.PowerState_PowerManagerDisabled)
	unavailable := &pb.RackTrayPowerDetails{}
	noPolicy := rackPowerSummaryMember(corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON, corev1.PowerState_On)
	noPolicy.Desired.GetValue().Status = corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_UNAVAILABLE
	noPolicy.Desired.GetValue().State = nil
	inapplicable := proto.Clone(noPolicy).(*pb.RackTrayPowerDetails)
	inapplicable.Desired.GetValue().Status = corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE
	missingTarget := proto.Clone(noPolicy).(*pb.RackTrayPowerDetails)
	missingTarget.Actual.GetValue().Outcome = &corev1.ComponentPowerObservation_Error{Error: &corev1.ComponentPowerError{Code: corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND}}
	paused := rackPowerSummaryMember(corev1.ComponentPowerState_COMPONENT_POWER_STATE_PAUSED, corev1.PowerState_On)
	malformed := rackPowerSummaryMember(corev1.ComponentPowerState(999), corev1.PowerState(999))

	tests := []struct {
		name                            string
		members                         []*pb.RackTrayPowerDetails
		actual, desired                 pb.RackPowerState
		actualCoverage, desiredCoverage *pb.RackPowerCoverage
	}{
		// Empty membership has no positive evidence of any state.
		{name: "empty", actualCoverage: &pb.RackPowerCoverage{}, desiredCoverage: &pb.RackPowerCoverage{}},
		// Equal known members provide a complete agreement, including On policy.
		{name: "agreement", members: []*pb.RackTrayPowerDetails{knownOn, knownOn}, actual: pb.RackPowerState_RACK_POWER_STATE_ON, desired: pb.RackPowerState_RACK_POWER_STATE_ON,
			actualCoverage: &pb.RackPowerCoverage{Total: 2, Known: 2}, desiredCoverage: &pb.RackPowerCoverage{Total: 2, Known: 2}},
		// A later On vote and missing member cannot erase a known On/Off conflict.
		{name: "known disagreement survives later members", members: []*pb.RackTrayPowerDetails{knownOn, knownOff, knownOn, unavailable}, actual: pb.RackPowerState_RACK_POWER_STATE_MIXED, desired: pb.RackPowerState_RACK_POWER_STATE_MIXED,
			actualCoverage: &pb.RackPowerCoverage{Total: 4, Known: 3, Unavailable: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 4, Known: 3, Unavailable: 1}},
		// Explicit hardware Unknown blocks actual agreement; disabled policy is
		// separately counted and is not a competing Off policy.
		{name: "unknown observation and disabled policy", members: []*pb.RackTrayPowerDetails{knownOn, unknownDisabled}, desired: pb.RackPowerState_RACK_POWER_STATE_ON,
			actualCoverage: &pb.RackPowerCoverage{Total: 2, Known: 1, Unknown: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 2, Known: 1, DisabledPolicy: 1}},
		// Missing reads block agreement independently of explicit Unknown.
		{name: "unavailable member", members: []*pb.RackTrayPowerDetails{knownOn, unavailable},
			actualCoverage: &pb.RackPowerCoverage{Total: 2, Known: 1, Unavailable: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 2, Known: 1, Unavailable: 1}},
		// Desired summarizes enabled policy only; switches and policy-free hosts
		// stay visible without becoming votes for either On or Off.
		{name: "policy coverage", members: []*pb.RackTrayPowerDetails{knownOn, noPolicy, inapplicable}, actual: pb.RackPowerState_RACK_POWER_STATE_ON, desired: pb.RackPowerState_RACK_POWER_STATE_ON,
			actualCoverage: &pb.RackPowerCoverage{Total: 3, Known: 3}, desiredCoverage: &pb.RackPowerCoverage{Total: 3, Known: 1, NoPolicy: 1, NotApplicable: 1}},
		// A stale Flow/Core link is not evidence that a current host has no
		// policy; Core NotFound must keep desired agreement indeterminate.
		{name: "missing managed target", members: []*pb.RackTrayPowerDetails{knownOn, missingTarget},
			actualCoverage: &pb.RackPowerCoverage{Total: 2, Known: 1, Unavailable: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 2, Known: 1, Unavailable: 1}},
		// No enabled policy is still Unknown even when its absence is readable.
		{name: "no applicable policy", members: []*pb.RackTrayPowerDetails{noPolicy, inapplicable, unknownDisabled},
			actualCoverage: &pb.RackPowerCoverage{Total: 3, Known: 2, Unknown: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 3, NoPolicy: 1, NotApplicable: 1, DisabledPolicy: 1}},
		// Transitional states retain their exact meaning rather than becoming On.
		{name: "paused observation", members: []*pb.RackTrayPowerDetails{paused}, actual: pb.RackPowerState_RACK_POWER_STATE_PAUSED, desired: pb.RackPowerState_RACK_POWER_STATE_ON,
			actualCoverage: &pb.RackPowerCoverage{Total: 1, Known: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 1, Known: 1}},
		// Unknown future enum values cannot count as known observed or desired states.
		{name: "unrecognized states", members: []*pb.RackTrayPowerDetails{malformed},
			actualCoverage: &pb.RackPowerCoverage{Total: 1, Unavailable: 1}, desiredCoverage: &pb.RackPowerCoverage{Total: 1, Unavailable: 1}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			actual, desired := summarizeRackPower(test.members)

			require.Equal(t, test.actual, actual.State)
			require.Equal(t, test.desired, desired.State)
			require.True(t, proto.Equal(test.actualCoverage, actual.Coverage), "actual coverage: %v", actual.Coverage)
			require.True(t, proto.Equal(test.desiredCoverage, desired.Coverage), "desired coverage: %v", desired.Coverage)
		})
	}
}

// rackPowerSummaryMember supplies explicit presence for observed state and
// desired policy, so default enum values cannot impersonate missing sections.
func rackPowerSummaryMember(actual corev1.ComponentPowerState, desired corev1.PowerState) *pb.RackTrayPowerDetails {
	return &pb.RackTrayPowerDetails{
		Actual:  &pb.PowerActualDetails{Result: &pb.PowerActualDetails_Value{Value: &corev1.ComponentPowerObservation{Outcome: &corev1.ComponentPowerObservation_State{State: actual}}}},
		Desired: &pb.PowerDesiredDetails{Result: &pb.PowerDesiredDetails_Value{Value: &corev1.ComponentPowerPolicy{Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE, State: &desired}}},
	}
}
