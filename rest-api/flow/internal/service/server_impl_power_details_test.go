// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"
	"net"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	nicoprovider "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providers/nico"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// powerDetailsCanonicalMachineID is a valid Core host ID with a 32-byte hash.
const powerDetailsCanonicalMachineID = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg"

// powerDetailsCoreReader records the one passive request made by a service test.
// The callback controls dependency failures without adding mutation methods.
type powerDetailsCoreReader struct {
	request  *corev1.GetComponentPowerDetailsRequest
	deadline time.Time
	read     func(context.Context) (*corev1.GetComponentPowerDetailsResponse, error)
}

// GetComponentPowerDetails captures identity and deadline before the controlled read.
func (r *powerDetailsCoreReader) GetComponentPowerDetails(ctx context.Context, req *corev1.GetComponentPowerDetailsRequest) (*corev1.GetComponentPowerDetailsResponse, error) {
	r.request = req
	r.deadline, _ = ctx.Deadline()
	return r.read(ctx)
}

// powerDetailsTaskStore permits only the bounded rack read. An accidental call
// to an unrelated store method fails the test through the embedded nil store.
type powerDetailsTaskStore struct {
	taskstore.Store
	rackID   uuid.UUID
	calls    int
	deadline time.Time
	read     func(context.Context) ([]taskdef.PowerTaskDetails, error)
}

// ListPowerTasksForRack counts calls so a skipped query differs from a nil-rack query.
func (s *powerDetailsTaskStore) ListPowerTasksForRack(ctx context.Context, rackID uuid.UUID) ([]taskdef.PowerTaskDetails, error) {
	s.rackID = rackID
	s.calls++
	s.deadline, _ = ctx.Deadline()
	return s.read(ctx)
}

// powerDetailsInventory records the resolver's budget before a test delays or
// fails inventory lookup. The embedded manager retains real selector behavior.
type powerDetailsInventory struct {
	*mockManager
	calls      int
	deadline   time.Time
	beforeRead func(context.Context) error
}

// GetComponentsByExternalIDs applies the test's delay or failure before resolution.
func (m *powerDetailsInventory) GetComponentsByExternalIDs(ctx context.Context, ids []string) ([]*component.Component, error) {
	m.calls++
	m.deadline, _ = ctx.Deadline()
	if m.beforeRead != nil {
		err := m.beforeRead(ctx)
		if err != nil {
			return nil, err
		}
	}
	return m.mockManager.GetComponentsByExternalIDs(ctx, ids)
}

// powerDetailsFixture keeps Flow identity separate from the managed Core ID so
// the public handler tests catch passing the wrong identity to either source.
type powerDetailsFixture struct {
	server       *FlowServerImpl
	tray         *component.Component
	core         *powerDetailsCoreReader
	store        *powerDetailsTaskStore
	entry        *corev1.ComponentPowerDetails
	inventory    *powerDetailsInventory
	wantTasks    *pb.PowerTaskDetailsList
	wantDeadline time.Time
}

// newPowerDetailsFixture supplies one linked compute tray and a successful empty
// action/task read. Scenarios alter only the boundary they intend to exercise.
func newPowerDetailsFixture() *powerDetailsFixture {
	tray := &component.Component{
		Type:        devicetypes.ComponentTypeCompute,
		Info:        deviceinfo.DeviceInfo{ID: uuid.New()},
		ComponentID: "managed-compute-id",
		RackID:      uuid.New(),
	}
	observedSource := corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_REDFISH
	policySource := corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_CORE_POWER_POLICY
	desiredState := corev1.PowerState_Off
	entry := &corev1.ComponentPowerDetails{
		ComponentId: tray.ComponentID,
		Actual: &corev1.ComponentPowerObservation{
			Outcome:    &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON},
			Source:     &observedSource,
			ObservedAt: timestamppb.New(time.Date(2026, 10, 15, 10, 0, 0, 0, time.UTC)),
		},
		Desired: &corev1.ComponentPowerPolicy{Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE, State: &desiredState, Source: &policySource},
		Actions: &corev1.ComponentPowerActionDetails{},
	}
	core := &powerDetailsCoreReader{read: func(context.Context) (*corev1.GetComponentPowerDetailsResponse, error) {
		return &corev1.GetComponentPowerDetailsResponse{Entries: []*corev1.ComponentPowerDetails{entry}}, nil
	}}
	store := &powerDetailsTaskStore{read: func(context.Context) ([]taskdef.PowerTaskDetails, error) { return nil, nil }}
	inventory := &powerDetailsInventory{mockManager: newMockManager()}
	inventory.components[tray.Info.ID] = tray
	inventory.componentsByMAC["aa:bb:cc:dd:ee:ff"] = tray
	return &powerDetailsFixture{
		server: &FlowServerImpl{inventoryManager: inventory, taskStore: store, powerReader: core},
		tray:   tray, core: core, store: store, entry: entry, inventory: inventory,
	}
}

// TestFlowServerImpl_GetTrayPowerDetails proves public identity wiring and
// independent availability, so a failed dependency cannot erase useful facts.
func TestFlowServerImpl_GetTrayPowerDetails(t *testing.T) {
	tests := []struct {
		name      string
		transport bool
		wantCode  codes.Code
		invoke    func(*testing.T, *powerDetailsFixture, context.Context, *pb.GetTrayPowerDetailsRequest) (*pb.GetTrayPowerDetailsResponse, error)
		configure func(*testing.T, *powerDetailsFixture) (context.Context, string)
		check     func(*testing.T, *powerDetailsFixture, *pb.GetTrayPowerDetailsResponse, error)
	}{
		{
			// Task targets are historical Flow UUIDs; a shelf task can affect this
			// tray without selecting it or implying an active Core action.
			name:      "linked compute retains typed selected and rack affected tasks",
			transport: true,
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				created := time.Date(2026, 10, 15, 10, 0, 0, 0, time.UTC)
				updated, started := created.Add(2*time.Minute), created.Add(time.Minute)
				// This shelf target is no longer in inventory, but its saved UUID remains meaningful.
				shelfID := uuid.MustParse("00000000-0000-0000-0000-000000000099")
				f.store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) {
					return []taskdef.PowerTaskDetails{
						{ID: uuid.MustParse("00000000-0000-0000-0000-000000000001"), RackID: f.tray.RackID,
							Operation:  operations.PowerControlTaskInfo{Operation: operations.PowerOperationForcePowerOff, Forced: true},
							Attributes: taskcommon.TaskAttributes{ComponentsByType: operation.ComponentsByType{devicetypes.ComponentTypeCompute: {f.tray.Info.ID}}},
							Status:     taskcommon.TaskStatusRunning, CreatedAt: created, UpdatedAt: updated, StartedAt: &started},
						{ID: uuid.MustParse("00000000-0000-0000-0000-000000000002"), RackID: f.tray.RackID,
							Operation:  operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
							Attributes: taskcommon.TaskAttributes{ComponentsByType: operation.ComponentsByType{devicetypes.ComponentTypePowerShelf: {shelfID}}},
							Status:     taskcommon.TaskStatusWaiting, CreatedAt: created, UpdatedAt: updated},
						{ID: uuid.New(), RackID: f.tray.RackID,
							Operation:  operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
							Attributes: taskcommon.TaskAttributes{ComponentsByType: operation.ComponentsByType{devicetypes.ComponentTypeCompute: {uuid.New()}}}},
					}, nil
				}
				f.wantTasks = &pb.PowerTaskDetailsList{Entries: []*pb.PowerTaskDetails{
					{Id: &pb.UUID{Id: "00000000-0000-0000-0000-000000000001"}, RackId: &pb.UUID{Id: f.tray.RackID.String()},
						Operation: &pb.TaskOperation{Operation: &pb.TaskOperation_PowerControl{PowerControl: &pb.PowerControlTaskOperation{Operation: pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF}}},
						Targets:   &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: f.tray.Info.ID.String()}}}}},
						Status:    pb.TaskStatus_TASK_STATUS_RUNNING, CreatedAt: timestamppb.New(created), UpdatedAt: timestamppb.New(updated), StartedAt: timestamppb.New(started),
						Relevance: pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED},
					{Id: &pb.UUID{Id: "00000000-0000-0000-0000-000000000002"}, RackId: &pb.UUID{Id: f.tray.RackID.String()},
						Operation: &pb.TaskOperation{Operation: &pb.TaskOperation_PowerControl{PowerControl: &pb.PowerControlTaskOperation{Operation: pb.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON}}},
						Targets:   &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_POWERSHELF, ComponentIds: []*pb.UUID{{Id: shelfID.String()}}}}},
						Status:    pb.TaskStatus_TASK_STATUS_WAITING, CreatedAt: timestamppb.New(created), UpdatedAt: timestamppb.New(updated),
						Relevance: pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED},
				}}
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				details := response.GetDetails()
				require.Equal(t, f.tray.Info.ID.String(), details.GetFlowComponentId().GetId())
				require.Equal(t, f.tray.ComponentID, details.GetCoreComponentId())
				require.Len(t, f.core.request.GetMachineIds().GetMachineIds(), 1)
				require.Equal(t, f.tray.ComponentID, f.core.request.GetMachineIds().GetMachineIds()[0].GetId())
				require.Equal(t, f.tray.RackID.String(), details.GetRackId().GetId())
				require.Equal(t, f.tray.RackID, f.store.rackID)
				require.True(t, proto.Equal(f.entry.Actual, details.GetActual().GetValue()))
				require.True(t, proto.Equal(f.entry.Desired, details.GetDesired().GetValue()))
				require.NotNil(t, details.GetOperations().GetActions().GetValue())
				require.True(t, proto.Equal(f.wantTasks, details.GetOperations().GetTasks()), "complete task metadata and recorded targets must survive")
			},
		},
		{
			// MAC normalization must reach the same managed switch identity.
			name: "MAC selector routes switch IDs",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.tray.Type = devicetypes.ComponentTypeNVSwitch
				return t.Context(), "AA-BB-CC-DD-EE-FF"
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.Len(t, f.core.request.GetSwitchIds().GetIds(), 1)
				require.Equal(t, f.tray.ComponentID, f.core.request.GetSwitchIds().Ids[0].GetId())
				require.Equal(t, pb.ComponentType_COMPONENT_TYPE_NVSWITCH, response.Details.ComponentType)
			},
		},
		{
			// Shelves use their own Core target oneof, never machine IDs.
			name: "power shelf target and absent rack",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.tray.Type, f.tray.RackID = devicetypes.ComponentTypePowerShelf, uuid.Nil
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.Len(t, f.core.request.GetPowerShelfIds().GetIds(), 1)
				require.Equal(t, f.tray.ComponentID, f.core.request.GetPowerShelfIds().Ids[0].GetId())
				require.Nil(t, response.Details.RackId)
				require.Zero(t, f.store.calls, "unassigned tray must not query an invented rack")
				require.Empty(t, response.Details.Operations.Tasks.Entries)
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED, response.Details.Operations.Tasks.GetError())
			},
		},
		{
			// Inventory is useful before Core ingestion creates the managed ID.
			name: "unlinked tray remains readable by MAC",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.tray.ComponentID = ""
				return t.Context(), "aa:bb:cc:dd:ee:ff"
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.Nil(t, response.Details.CoreComponentId)
				require.Nil(t, f.core.request)
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED, response.Details.Actual.GetError())
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED, response.Details.Desired.GetError())
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED, response.Details.Operations.Actions.GetError())
				require.Nil(t, response.Details.Operations.Tasks.Error)
				require.Equal(t, f.tray.RackID, f.store.rackID)
			},
		},
		{
			// A missing provider is a section failure, not a service outage.
			name: "no NICo provider",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.server.powerReader = nil
				created := f.entry.Actual.ObservedAt.AsTime()
				f.store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) {
					return []taskdef.PowerTaskDetails{{
						ID: uuid.MustParse("00000000-0000-0000-0000-000000000003"), RackID: f.tray.RackID,
						Operation:  operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
						Attributes: taskcommon.TaskAttributes{ComponentsByType: operation.ComponentsByType{devicetypes.ComponentTypeCompute: {f.tray.Info.ID}}},
						Status:     taskcommon.TaskStatusPending, CreatedAt: created, UpdatedAt: created,
					}}, nil
				}
				f.wantTasks = &pb.PowerTaskDetailsList{Entries: []*pb.PowerTaskDetails{{
					Id: &pb.UUID{Id: "00000000-0000-0000-0000-000000000003"}, RackId: &pb.UUID{Id: f.tray.RackID.String()},
					Operation: &pb.TaskOperation{Operation: &pb.TaskOperation_PowerControl{PowerControl: &pb.PowerControlTaskOperation{Operation: pb.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON}}},
					Targets:   &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: f.tray.Info.ID.String()}}}}},
					Status:    pb.TaskStatus_TASK_STATUS_PENDING, CreatedAt: timestamppb.New(created), UpdatedAt: timestamppb.New(created),
					Relevance: pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED,
				}}}
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.Nil(t, f.core.request)
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED, response.Details.Actual.GetError())
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED, response.Details.Desired.GetError())
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED, response.Details.Operations.Actions.GetError())
				require.True(t, proto.Equal(f.wantTasks, response.Details.Operations.Tasks), "selected task survives missing Core")
			},
		},
		{
			// Old Core servers lack the method; task availability is independent.
			name: "older Core method unsupported",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.core.read = func(context.Context) (*corev1.GetComponentPowerDetailsResponse, error) {
					return nil, status.Error(codes.Unimplemented, "sensitive dependency payload")
				}
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, _ *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED, response.Details.Actual.GetError())
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED, response.Details.Desired.GetError())
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED, response.Details.Operations.Actions.GetError())
				require.NotContains(t, response.String(), "sensitive")
				require.Nil(t, response.Details.Operations.Tasks.Error)
			},
		},
		{
			// Task storage errors must not erase successfully observed Core facts.
			name: "task failure preserves Core",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) { return nil, errors.New("sensitive SQL") }
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.True(t, proto.Equal(f.entry.Actual, response.Details.Actual.GetValue()))
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED, response.Details.Operations.Tasks.GetError())
				require.NotContains(t, response.String(), "sensitive")
			},
		},
		{
			// Overflow is a distinct unavailable section, never an empty success.
			name: "rack task limit",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) {
					return nil, &taskcommon.PowerTaskLimitError{RackID: f.tray.RackID}
				}
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.True(t, proto.Equal(f.entry.Desired, response.Details.Desired.GetValue()))
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED, response.Details.Operations.Tasks.GetError())
				require.Empty(t, response.Details.Operations.Tasks.Entries)
			},
		},
		{
			// A MAC-looking external ID can conflict with another member's MAC.
			name: "ambiguous identity stops both sources",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				other := &component.Component{Type: f.tray.Type, Info: deviceinfo.DeviceInfo{ID: uuid.New()}, ComponentID: "aa:bb:cc:dd:ee:ff"}
				f.inventory.components[other.Info.ID] = other
				return t.Context(), other.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.Equal(t, codes.FailedPrecondition, status.Code(err))
				require.Nil(t, response)
				require.Nil(t, f.core.request)
				require.Zero(t, f.store.calls)
			},
		},
		{
			// The handler must preserve the resolver's required-selector error.
			name: "empty identifier", wantCode: codes.InvalidArgument,
			configure: func(t *testing.T, _ *powerDetailsFixture) (context.Context, string) { return t.Context(), "" },
		},
		{
			// Missing inventory is a caller-visible absence, not a dependency outage.
			name: "unknown identifier", wantCode: codes.NotFound,
			configure: func(t *testing.T, _ *powerDetailsFixture) (context.Context, string) {
				return t.Context(), "absent-tray"
			},
		},
		{
			// An inventory component without a supported Core target stops here.
			name: "unsupported component type", wantCode: codes.InvalidArgument,
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.tray.Type = devicetypes.ComponentTypeToRSwitch
				return t.Context(), f.tray.ComponentID
			},
		},
		{
			// Dependency diagnostics never become a public error payload.
			name: "inventory dependency unavailable", wantCode: codes.Unavailable,
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.inventory.beforeRead = func(context.Context) error { return errors.New("sensitive inventory payload") }
				return t.Context(), f.tray.ComponentID
			},
		},
		{
			// A deterministic inventory defect retains Internal after sanitization.
			name: "inventory internal failure", wantCode: codes.Internal,
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.inventory.beforeRead = func(context.Context) error { return status.Error(codes.Internal, "sensitive inventory payload") }
				return t.Context(), f.tray.ComponentID
			},
		},
		{
			// Inventory consumes the same default budget as both later source reads.
			name: "default deadline includes inventory",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				f.wantDeadline = time.Now().Add(30 * time.Second)
				f.inventory.beforeRead = func(context.Context) error { time.Sleep(2 * time.Second); return nil }
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, _ *powerDetailsFixture, _ *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
			},
		},
		{
			// With less than the one-second reserve, even inventory must be skipped.
			name: "caller budget already reserved",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				ctx, cancel := context.WithTimeout(t.Context(), 500*time.Millisecond)
				t.Cleanup(cancel)
				return ctx, f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.Equal(t, codes.DeadlineExceeded, status.Code(err))
				require.Nil(t, response)
				require.Zero(t, f.inventory.calls)
				require.Nil(t, f.core.request)
				require.Zero(t, f.store.calls)
			},
		},
		{
			// One selected task can exceed the byte limit despite satisfying the row cap.
			name: "oversized task section retains Core",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				targets := make([]uuid.UUID, 45000)
				for i := range targets {
					targets[i] = uuid.MustParse(fmt.Sprintf("00000000-0000-0000-0000-%012x", i))
				}
				targets[0] = f.tray.Info.ID
				f.store.read = func(context.Context) ([]taskdef.PowerTaskDetails, error) {
					return []taskdef.PowerTaskDetails{{
						ID: uuid.New(), RackID: f.tray.RackID, Operation: operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
						Attributes: taskcommon.TaskAttributes{ComponentsByType: operation.ComponentsByType{devicetypes.ComponentTypeCompute: targets}},
					}}, nil
				}
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED, response.Details.Operations.Tasks.GetError())
				require.Empty(t, response.Details.Operations.Tasks.Entries)
				require.True(t, proto.Equal(f.entry.Actual, response.Details.Actual.GetValue()))
				require.True(t, proto.Equal(f.entry.Desired, response.Details.Desired.GetValue()))
				require.True(t, proto.Equal(f.entry.Actions, response.Details.Operations.Actions.GetValue()))
			},
		},
		{
			// Removing tasks cannot fit oversized Core facts; the RPC must return the size error.
			name: "Core alone exceeds allowance",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				initiator := strings.Repeat("x", 1991681)
				f.entry.Actions = &corev1.ComponentPowerActionDetails{
					Actions: []*corev1.ComponentPowerAction{{Initiator: &initiator}},
				}
				return t.Context(), f.tray.ComponentID
			},
			check: func(t *testing.T, _ *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.Equal(t, codes.ResourceExhausted, status.Code(err))
				require.Nil(t, response)
			},
		},
		{
			// A child deadline leaves enough caller budget to return completed facts.
			name: "task deadline preserves completed Core section",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
				t.Cleanup(cancel)
				f.wantDeadline = time.Now().Add(9 * time.Second)
				f.inventory.beforeRead = func(context.Context) error { time.Sleep(2 * time.Second); return nil }
				f.store.read = func(ctx context.Context) ([]taskdef.PowerTaskDetails, error) { <-ctx.Done(); return nil, ctx.Err() }
				return ctx, f.tray.ComponentID
			},
			check: func(t *testing.T, f *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.NoError(t, err)
				require.True(t, proto.Equal(f.entry.Actual, response.Details.Actual.GetValue()))
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED, response.Details.Operations.Tasks.GetError())
			},
		},
		{
			// A blocked source must finish its cleanup before cancellation returns.
			name: "caller cancellation joins both reads",
			configure: func(t *testing.T, f *powerDetailsFixture) (context.Context, string) {
				return t.Context(), f.tray.ComponentID
			},
			invoke: func(t *testing.T, f *powerDetailsFixture, ctx context.Context, request *pb.GetTrayPowerDetailsRequest) (*pb.GetTrayPowerDetailsResponse, error) {
				ctx, cancel := context.WithCancel(ctx)
				started, canceled, completed := make(chan struct{}, 2), make(chan struct{}, 2), make(chan struct{}, 2)
				releaseCore, releaseTasks := make(chan struct{}, 1), make(chan struct{}, 1)
				defer func() {
					cancel()
					close(releaseCore)
					close(releaseTasks)
				}()
				f.core.read = func(ctx context.Context) (*corev1.GetComponentPowerDetailsResponse, error) {
					started <- struct{}{}
					<-ctx.Done()
					canceled <- struct{}{}
					<-releaseCore
					completed <- struct{}{}
					return nil, ctx.Err()
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
					response *pb.GetTrayPowerDetailsResponse
					err      error
				}
				returned := make(chan result, 1)
				go func() {
					response, err := f.server.GetTrayPowerDetails(ctx, request)
					returned <- result{response, err}
				}()

				// Wait diagnoses a sequential implementation without an unbounded start wait.
				synctest.Wait()
				require.Len(t, started, 2)
				cancel()
				synctest.Wait()
				require.Len(t, canceled, 2)
				require.Empty(t, returned)
				releaseCore <- struct{}{}
				synctest.Wait()
				require.Len(t, completed, 1)
				require.Empty(t, returned, "RPC must join the task read after the Core read finishes")

				releaseTasks <- struct{}{}
				got := <-returned
				require.Len(t, completed, 2, "both callbacks completed before RPC return")
				return got.response, got.err
			},
			check: func(t *testing.T, _ *powerDetailsFixture, response *pb.GetTrayPowerDetailsResponse, err error) {
				require.Nil(t, response)
				require.Equal(t, codes.Canceled, status.Code(err))
			},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			run := func(t *testing.T) {
				f := newPowerDetailsFixture()
				ctx, selector := test.configure(t, f)

				request := &pb.GetTrayPowerDetailsRequest{Id: selector}
				var response *pb.GetTrayPowerDetailsResponse
				var err error
				switch {
				case test.invoke != nil:
					response, err = test.invoke(t, f, ctx, request)
				case test.transport:
					response, err = powerDetailsRPCClient(t, f.server).GetTrayPowerDetails(ctx, request)
				default:
					response, err = f.server.GetTrayPowerDetails(ctx, request)
				}

				if test.check != nil {
					test.check(t, f, response, err)
				} else {
					require.Equal(t, test.wantCode, status.Code(err))
					require.Nil(t, response)
					require.Nil(t, f.core.request)
					require.Zero(t, f.store.calls)
					require.NotContains(t, err.Error(), "sensitive")
				}
				if !f.wantDeadline.IsZero() {
					require.Equal(t, f.wantDeadline, f.inventory.deadline)
					require.Equal(t, f.wantDeadline, f.core.deadline)
					require.Equal(t, f.wantDeadline, f.store.deadline)
				}
			}
			if test.transport {
				run(t)
			} else {
				// Direct fakes use Go's clock and blocking detector; no wall-time sleeps.
				synctest.Test(t, run)
			}
		})
	}
}

// TestFlowServerImpl_readCorePowerDetails protects identity and message presence
// at the narrow Core boundary without duplicating its hardware/state test matrix.
func TestFlowServerImpl_readCorePowerDetails(t *testing.T) {
	tests := []struct {
		name                                 string
		componentID                          string
		nilResponse                          bool
		mutate                               func(*corev1.GetComponentPowerDetailsResponse)
		wantActual, wantDesired, wantActions pb.PowerDetailsReadError
	}{
		// Ingestion preserves hash case, while Core replies with canonical identity.
		{
			name:        "canonical Core ID matches uppercase recorded hash",
			componentID: powerDetailsCanonicalMachineID[:7] + strings.ToUpper(powerDetailsCanonicalMachineID[7:]),
			mutate: func(r *corev1.GetComponentPowerDetailsResponse) {
				r.Entries[0].ComponentId = powerDetailsCanonicalMachineID
			},
		},
		// A failed observation cannot erase saved action metadata or desired policy.
		{
			name: "hardware failure retains tracked action metadata",
			mutate: func(r *corev1.GetComponentPowerDetailsResponse) {
				entry := r.Entries[0]
				source := corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_RMS
				entry.Actual = &corev1.ComponentPowerObservation{Source: &source, Outcome: &corev1.ComponentPowerObservation_Error{
					Error: &corev1.ComponentPowerError{Code: corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_ENDPOINT_UNAVAILABLE, Message: "endpoint is unavailable"},
				}}
				entry.Actions = &corev1.ComponentPowerActionDetails{Actions: []*corev1.ComponentPowerAction{
					{Status: corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_ACTIVE,
						Operation:   corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_OFF,
						RequestedAt: timestamppb.New(time.Date(2026, 10, 15, 10, 0, 0, 0, time.UTC)), Initiator: proto.String(""), Graceful: proto.Bool(false)},
					{Status: corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_PENDING,
						Operation:   corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_CHASSIS_RESET,
						RequestedAt: timestamppb.New(time.Date(2026, 10, 15, 10, 1, 0, 0, time.UTC)), Initiator: proto.String("admin"), ChassisId: proto.String("enclosure-1")},
				}}
			},
		},
		// Core's nested NotFound remains a value with an error, never empty success.
		{
			name: "missing managed target retains nested failures",
			mutate: func(r *corev1.GetComponentPowerDetailsResponse) {
				entry := r.Entries[0]
				entry.Actual = &corev1.ComponentPowerObservation{Outcome: &corev1.ComponentPowerObservation_Error{
					Error: &corev1.ComponentPowerError{Code: corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND, Message: "managed target not found"},
				}}
				entry.Desired = &corev1.ComponentPowerPolicy{Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_UNAVAILABLE}
				entry.Actions = &corev1.ComponentPowerActionDetails{Error: &corev1.ComponentPowerError{
					Code: corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND, Message: "managed target not found",
				}}
			},
		},
		// Wrong identity invalidates every Core section, even a plausible value.
		{
			name:        "mismatched ID",
			mutate:      func(r *corev1.GetComponentPowerDetailsResponse) { r.Entries[0].ComponentId = "other" },
			wantActual:  pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
			wantDesired: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
			wantActions: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
		},
		// Duplicate results cannot satisfy the exactly-one response contract.
		{
			name:        "duplicate entries",
			mutate:      func(r *corev1.GetComponentPowerDetailsResponse) { r.Entries = append(r.Entries, r.Entries[0]) },
			wantActual:  pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
			wantDesired: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
			wantActions: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
		},
		// A missing response must fail all sections without indexing absent entries.
		{
			name:        "nil Core response",
			nilResponse: true,
			wantActual:  pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
			wantDesired: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
			wantActions: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
		},
		// A missing actual result must not look like the default Unknown state.
		{
			name: "missing observation",
			mutate: func(r *corev1.GetComponentPowerDetailsResponse) {
				r.Entries[0].Actual = &corev1.ComponentPowerObservation{}
			},
			wantActual: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
		},
		// A missing policy is different from an explicit Unavailable policy.
		{
			name:        "missing policy",
			mutate:      func(r *corev1.GetComponentPowerDetailsResponse) { r.Entries[0].Desired = nil },
			wantDesired: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE,
		},
		// Servers before action support omit the message rather than sending empty.
		{
			name:        "older Core actions",
			mutate:      func(r *corev1.GetComponentPowerDetailsResponse) { r.Entries[0].Actions = nil },
			wantActions: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED,
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			f := newPowerDetailsFixture()
			if test.componentID != "" {
				f.tray.ComponentID = test.componentID
			}
			response := &corev1.GetComponentPowerDetailsResponse{Entries: []*corev1.ComponentPowerDetails{f.entry}}
			if test.nilResponse {
				response = nil
			}
			if test.mutate != nil {
				test.mutate(response)
			}
			want := proto.Clone(f.entry).(*corev1.ComponentPowerDetails)
			f.core.read = func(context.Context) (*corev1.GetComponentPowerDetailsResponse, error) { return response, nil }

			actual, desired, actions := f.server.readCorePowerDetails(t.Context(), f.tray)

			require.Len(t, f.core.request.GetMachineIds().GetMachineIds(), 1)
			require.Equal(t, f.tray.ComponentID, f.core.request.GetMachineIds().GetMachineIds()[0].GetId())
			require.Equal(t, test.wantActual, actual.GetError())
			require.Equal(t, test.wantDesired, desired.GetError())
			require.Equal(t, test.wantActions, actions.GetError())
			require.Equal(t, test.wantActual == 0, actual.GetValue() != nil)
			require.Equal(t, test.wantDesired == 0, desired.GetValue() != nil)
			require.Equal(t, test.wantActions == 0, actions.GetValue() != nil)
			if test.wantActual == 0 {
				require.True(t, proto.Equal(want.Actual, actual.GetValue()), "preserve complete hardware evidence")
			}
			if test.wantDesired == 0 {
				require.True(t, proto.Equal(want.Desired, desired.GetValue()), "preserve policy presence and status")
			}
			if test.wantActions == 0 {
				require.True(t, proto.Equal(want.Actions, actions.GetValue()), "preserve actions, metadata and nested error")
			}
		})
	}
}

// TestSameCorePowerIdentity permits Core's hash normalization without matching a
// different typed identity or treating malformed Base32 suffixes as equivalent.
func TestSameCorePowerIdentity(t *testing.T) {
	canonical := powerDetailsCanonicalMachineID
	uppercase := canonical[:7] + strings.ToUpper(canonical[7:])
	badTrailingBits := canonical[:58] + "h"
	tests := []struct {
		name, expected, observed string
		want                     bool
	}{
		// Equal future formats retain the existing exact-match behavior.
		{name: "exact opaque identity", expected: "future-id", observed: "future-id", want: true},
		// Only the hardware hash is case-insensitive in Core's representation.
		{name: "uppercase hash", expected: uppercase, observed: canonical, want: true},
		// Host and DPU type bytes remain distinct even for identical hardware hashes.
		{name: "different typed prefix", expected: "fm100dt" + uppercase[7:], observed: canonical},
		// A valid but different 32-byte hash must not satisfy identity validation.
		{name: "different hardware hash", expected: uppercase, observed: canonical[:7] + "1" + canonical[8:]},
		// Nonzero unused trailing bits are not a valid encoding of the hardware hash.
		{name: "invalid trailing bits", expected: badTrailingBits[:7] + strings.ToUpper(badTrailingBits[7:]), observed: badTrailingBits},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			require.Equal(t, test.want, sameCorePowerIdentity(test.expected, test.observed))
		})
	}
}

// TestPowerReaderFromRegistry ensures production construction borrows the exact
// configured client and allows a Flow deployment with no NICo provider.
func TestPowerReaderFromRegistry(t *testing.T) {
	for _, configured := range []bool{false, true} {
		t.Run(map[bool]string{false: "no provider", true: "shared provider client"}[configured], func(t *testing.T) {
			registry := providerapi.NewProviderRegistry()
			client := nicoapi.NewMockClient()
			if configured {
				require.NoError(t, registry.Register(nicoprovider.NewFromClient(client)))
			}

			reader := powerReaderFromRegistry(registry)
			server, err := newServerImplementation(nil, nil, nil, nil, nil, nil, nil, nil, reader)
			require.NoError(t, err)
			require.Equal(t, reader, server.powerReader)

			if configured {
				require.Same(t, client, reader)
			} else {
				require.Nil(t, reader)
			}
		})
	}
}

// powerDetailsRPCClient exercises the generated method registration on an
// in-memory connection; no test listener or credentials are shared with a site.
func powerDetailsRPCClient(t *testing.T, implementation *FlowServerImpl) pb.FlowClient {
	t.Helper()
	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	pb.RegisterFlowServer(server, implementation)
	finished := make(chan struct{})
	go func() {
		defer close(finished)
		_ = server.Serve(listener)
	}()
	connection, err := grpc.NewClient("passthrough:///power-details",
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) { return listener.Dial() }),
	)
	require.NoError(t, err)
	t.Cleanup(func() {
		require.NoError(t, connection.Close())
		server.Stop()
		require.NoError(t, listener.Close())
		<-finished
	})
	return pb.NewFlowClient(connection)
}

// TestBoundPowerDetailsResponse verifies all-or-nothing task availability at
// the actual proxy encoding boundary, including JSON's HTML escape expansion.
func TestBoundPowerDetailsResponse(t *testing.T) {
	tests := []struct {
		name           string
		targetCount    int
		initiator      string
		wantTasksError bool
	}{
		// A small list remains available and is not accidentally suppressed.
		{name: "ordinary response", targetCount: 1},
		// '<' is one protojson byte but six bytes in the proxy's JSON wrapper.
		{name: "HTML expansion exceeds proxy allowance", targetCount: 10000, initiator: strings.Repeat("<", 270000), wantTasksError: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			targets := make([]*pb.UUID, test.targetCount)
			for index := range targets {
				targets[index] = &pb.UUID{Id: fmt.Sprintf("00000000-0000-0000-0000-%012x", index)}
			}
			response := &pb.GetTrayPowerDetailsResponse{Details: &pb.TrayPowerDetails{
				Operations: &pb.PowerOperations{
					Actions: &pb.PowerActionDetails{Result: &pb.PowerActionDetails_Value{Value: &corev1.ComponentPowerActionDetails{
						Actions: []*corev1.ComponentPowerAction{{Initiator: &test.initiator}},
					}}},
					Tasks: &pb.PowerTaskDetailsList{Entries: []*pb.PowerTaskDetails{{
						Targets: &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{ComponentIds: targets}}},
					}}},
				},
			}}

			err := boundPowerDetailsResponse(response)

			require.NoError(t, err)
			if test.wantTasksError {
				require.Equal(t, pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED, response.Details.Operations.Tasks.GetError())
				require.Empty(t, response.Details.Operations.Tasks.Entries)
			} else {
				require.Nil(t, response.Details.Operations.Tasks.Error)
				require.Len(t, response.Details.Operations.Tasks.Entries, 1)
			}
			require.Len(t, response.Details.Operations.Actions.GetValue().Actions, 1)
		})
	}
}
