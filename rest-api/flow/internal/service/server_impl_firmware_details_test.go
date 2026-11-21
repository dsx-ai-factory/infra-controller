// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// firmwareDetailsCoreReader records passive requests and permits precise failure
// or cancellation injection. No mutation capability is available to the service.
type firmwareDetailsCoreReader struct {
	request  *corev1.GetComponentFirmwareDetailsRequest
	deadline time.Time
	read     func(context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error)
}

// GetComponentFirmwareDetails records the exact Core identity and shared deadline.
func (r *firmwareDetailsCoreReader) GetComponentFirmwareDetails(ctx context.Context, req *corev1.GetComponentFirmwareDetailsRequest) (*corev1.GetComponentFirmwareDetailsResponse, error) {
	r.request = req
	r.deadline, _ = ctx.Deadline()
	return r.read(ctx)
}

// firmwareDetailsTaskStore admits only the firmware projection and optional
// context read. Any unexpected mutation panics through the embedded nil store.
type firmwareDetailsTaskStore struct {
	taskstore.Store
	calls        int
	rackID       uuid.UUID
	deadline     time.Time
	contextCalls int
	read         func(context.Context) ([]taskdef.FirmwareTaskDetails, error)
	contextRead  func(context.Context) ([]taskdef.TaskContextMetadata, error)
}

// ListFirmwareTasksForRack records one complete bounded query for the current rack.
func (s *firmwareDetailsTaskStore) ListFirmwareTasksForRack(ctx context.Context, id uuid.UUID) ([]taskdef.FirmwareTaskDetails, error) {
	s.calls++
	s.rackID = id
	s.deadline, _ = ctx.Deadline()
	return s.read(ctx)
}

// ListTaskContextForRack distinguishes a skipped optional read from an empty result.
func (s *firmwareDetailsTaskStore) ListTaskContextForRack(ctx context.Context, _ uuid.UUID) ([]taskdef.TaskContextMetadata, error) {
	s.contextCalls++
	if s.contextRead != nil {
		return s.contextRead(ctx)
	}
	return nil, nil
}

// firmwareDetailsFixture separates inventory/Flow IDs, installed/expected
// versions and request times so incorrect source or field reuse is observable.
type firmwareDetailsFixture struct {
	server    *FlowServerImpl
	tray      *component.Component
	inventory *detailsInventory
	core      *firmwareDetailsCoreReader
	store     *firmwareDetailsTaskStore
	entry     *corev1.ComponentFirmwareDetails
	tasks     []taskdef.FirmwareTaskDetails
}

// newFirmwareDetailsFixture supplies distinct installed/expected/requested versions
// and a fully linked task; individual cases alter only their claimed boundary.
func newFirmwareDetailsFixture() *firmwareDetailsFixture {
	tray := &component.Component{
		Type:           devicetypes.ComponentTypeCompute,
		Info:           deviceinfo.DeviceInfo{ID: uuid.UUID{15: 1}},
		ComponentID:    powerDetailsCanonicalMachineID,
		RackID:         uuid.UUID{15: 2},
		RackExternalID: "inventory-rack",
	}
	installed := &corev1.ComponentFirmwareInventoryEntry{
		InventoryId: proto.String("/Inventory/BIOS"),
		Result: &corev1.ComponentFirmwareInventoryEntry_Component{
			Component: &corev1.ComponentFirmwareInstalledComponent{
				Name: proto.String("BIOS"), Version: proto.String("installed-1"),
			},
		},
	}
	entry := &corev1.ComponentFirmwareDetails{
		ComponentId: tray.ComponentID,
		Actual: &corev1.ComponentFirmwareInstalled{
			Source:     corev1.ComponentFirmwareSource_COMPONENT_FIRMWARE_SOURCE_REDFISH.Enum(),
			ObservedAt: timestamppb.New(time.Unix(100, 0)),
			Result: &corev1.ComponentFirmwareInstalled_Inventory{
				Inventory: &corev1.ComponentFirmwareInventory{Entries: []*corev1.ComponentFirmwareInventoryEntry{installed}},
			},
		},
		Desired: &corev1.ComponentFirmwareExpected{
			Source: corev1.ComponentFirmwareSource_COMPONENT_FIRMWARE_SOURCE_EFFECTIVE_HOST_CONFIGURATION.Enum(),
			Result: &corev1.ComponentFirmwareExpected_Host{
				Host: &corev1.ComponentHostFirmwareExpectations{
					Vendor: "vendor", Model: "model",
					Entries: []*corev1.ComponentHostFirmwareExpectation{{Component: "BIOS", Version: "expected-2"}},
				},
			},
		},
		Actions: &corev1.ComponentFirmwareActions{
			Stored: &corev1.ComponentStoredFirmwareActions{
				Current: []*corev1.ComponentCurrentFirmwareWork{{
					Target: &corev1.ComponentFirmwareWorkTarget{Target: &corev1.ComponentFirmwareWorkTarget_Host{
						Host: &corev1.ComponentHostFirmwareTarget{Component: proto.String("BIOS"), Version: proto.String("frozen-0")},
					}},
				}},
			},
			Backend: &corev1.ComponentFirmwareBackendWork{
				Error: &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_POLL_FAILED, Message: "firmware job could not be read"},
			},
		},
	}
	f := &firmwareDetailsFixture{tray: tray, entry: entry, inventory: &detailsInventory{mockManager: newMockManager()}, core: &firmwareDetailsCoreReader{}, store: &firmwareDetailsTaskStore{}}
	f.inventory.components[tray.Info.ID] = tray
	f.inventory.componentsByMAC["aa:bb:cc:dd:ee:ff"] = tray
	f.tasks = []taskdef.FirmwareTaskDetails{{ID: uuid.UUID{15: 3}, RackID: tray.RackID, Operation: operations.FirmwareOperationUpgrade, Status: taskcommon.TaskStatusPending,
		Attributes:       taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{tray.Type: {tray.Info.ID}}},
		RequestedTargets: taskdef.FirmwareRequestedTargets{Shared: &taskdef.FirmwareTargetMetadata{Kind: taskdef.FirmwareTargetVersion, Value: "requested-3"}}, CreatedAt: time.Unix(200, 0), UpdatedAt: time.Unix(250, 0)}}
	f.core.read = func(context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) {
		return &corev1.GetComponentFirmwareDetailsResponse{Entries: []*corev1.ComponentFirmwareDetails{f.entry}}, nil
	}
	f.store.read = func(context.Context) ([]taskdef.FirmwareTaskDetails, error) { return f.tasks, nil }
	f.server = &FlowServerImpl{inventoryManager: f.inventory, firmwareReader: f.core, taskStore: f.store}
	return f
}

// TestFlowServerImpl_GetTrayFirmwareDetails proves composition and failure
// independence at the public service boundary, without repeating backend parsers.
func TestFlowServerImpl_GetTrayFirmwareDetails(t *testing.T) {
	tests := []struct {
		name      string
		configure func(*firmwareDetailsFixture)
		check     func(*testing.T, *firmwareDetailsFixture, *pb.TrayFirmwareDetails)
	}{
		// A populated response must retain source identity and distinct version meanings.
		{name: "populated compute", check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Equal(t, f.tray.ComponentID, d.GetComponentId())
			require.Equal(t, pb.ComponentType_COMPONENT_TYPE_COMPUTE, d.ComponentType)
			require.Equal(t, "inventory-rack", d.GetRackExternalId())
			require.Equal(t, f.tray.RackID.String(), d.GetRackId().GetId())
			require.Equal(t, f.tray.ComponentID, f.core.request.GetMachineIds().GetMachineIds()[0].GetId())
			require.True(t, proto.Equal(f.entry.Actual, d.Actual.GetValue()))
			require.True(t, proto.Equal(f.entry.Desired, d.Desired.GetValue()))
			require.True(t, proto.Equal(f.entry.Actions, d.Operations.Actions.GetValue()))
			require.Equal(t, f.tray.RackID, f.store.rackID)
			require.Equal(t, 1, f.inventory.identityCalls)
			require.Equal(t, []uuid.UUID{f.tray.Info.ID}, f.inventory.identityIDs)
			require.Equal(t, f.core.deadline, f.store.deadline)
			require.Equal(t, f.core.deadline, f.inventory.identityDeadline)

			wantTask := &pb.FirmwareTaskDetails{
				Id:        &pb.UUID{Id: "00000000-0000-0000-0000-000000000003"},
				RackId:    &pb.UUID{Id: f.tray.RackID.String()},
				Operation: pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE,
				Status:    pb.TaskStatus_TASK_STATUS_PENDING,
				Targets:   &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: f.tray.Info.ID.String()}}}}},
				CreatedAt: timestamppb.New(time.Unix(200, 0)), UpdatedAt: timestamppb.New(time.Unix(250, 0)),
				RequestedTargets: &pb.FirmwareRequestedTargets{Selection: &pb.FirmwareRequestedTargets_Shared{Shared: &pb.FirmwareTargetMetadata{Kind: pb.FirmwareTargetKind_FIRMWARE_TARGET_KIND_VERSION, Value: "requested-3"}}},
				ExternalTargets:  &pb.FirmwareTaskExternalTargets{RackId: "inventory-rack", Targets: []*pb.ExternalRef{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, Id: f.tray.ComponentID}}},
			}
			require.Len(t, d.Operations.Tasks.Entries, 1)
			require.True(t, proto.Equal(wantTask, d.Operations.Tasks.Entries[0]), "complete stored task fields and identities must survive")

			require.Zero(t, f.store.contextCalls)
		}},
		// The same public selector must use the switch oneof rather than machine IDs.
		{name: "switch target", configure: func(f *firmwareDetailsFixture) {
			f.tray.Type = devicetypes.ComponentTypeNVSwitch
			f.tray.ComponentID = "sw100nt" + f.tray.ComponentID[7:]
			f.entry.ComponentId = f.tray.ComponentID
			f.tasks = nil
		}, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Len(t, f.core.request.GetSwitchIds().GetIds(), 1)
			require.Equal(t, f.tray.ComponentID, f.core.request.GetSwitchIds().GetIds()[0].GetId())
			require.Equal(t, pb.ComponentType_COMPONENT_TYPE_NVSWITCH, d.ComponentType)
		}},
		// Shelf firmware requires its dedicated Core target, without a power operation.
		{name: "shelf target", configure: func(f *firmwareDetailsFixture) {
			f.tray.Type = devicetypes.ComponentTypePowerShelf
			f.tray.ComponentID = "ps100rt" + f.tray.ComponentID[7:]
			f.entry.ComponentId = f.tray.ComponentID
			f.tasks = nil
		}, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Len(t, f.core.request.GetPowerShelfIds().GetIds(), 1)
			require.Equal(t, f.tray.ComponentID, f.core.request.GetPowerShelfIds().GetIds()[0].GetId())
			require.Equal(t, pb.ComponentType_COMPONENT_TYPE_POWERSHELF, d.ComponentType)
		}},
		// Core absence must not conceal stored firmware intent readable by MAC.
		{name: "MAC before Core linkage", configure: func(f *firmwareDetailsFixture) { f.tray.ComponentID = "" }, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Nil(t, f.core.request)
			require.Nil(t, d.ComponentId)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED, d.Desired.GetError())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED, d.Operations.Actions.GetError())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED, d.Actual.GetError())
			require.Len(t, d.Operations.Tasks.Entries, 1)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED, d.Operations.Tasks.Entries[0].ExternalTargets.GetError())
		}},
		// Missing rack assignment must skip the task-store query and preserve Core data.
		{name: "rackless tray", configure: func(f *firmwareDetailsFixture) { f.tray.RackID = uuid.Nil }, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Zero(t, f.store.calls)
			require.Nil(t, d.RackId)
			require.Nil(t, d.RackExternalId)
			require.NotNil(t, d.Actual.GetValue())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED, d.Operations.Tasks.GetError())
		}},
		// Missing configuration cannot masquerade as successful empty Core evidence.
		{name: "no NICo provider", configure: func(f *firmwareDetailsFixture) {
			f.server.firmwareReader = nil
		}, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Nil(t, f.core.request)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED, d.Actual.GetError())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED, d.Desired.GetError())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED, d.Operations.Actions.GetError())
			require.Len(t, d.Operations.Tasks.Entries, 1)
		}},
		// Sanitized store conversion failures use the same category as other read failures.
		{name: "invalid stored task data", configure: func(f *firmwareDetailsFixture) {
			f.store.read = func(context.Context) ([]taskdef.FirmwareTaskDetails, error) {
				return nil, status.Error(codes.Internal, "private-stored-task")
			}
		}, check: func(t *testing.T, _ *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, d.Operations.Tasks.GetError())
			require.Empty(t, d.Operations.Tasks.Entries)
			require.NotNil(t, d.Actual.GetValue())
		}},
		// The public handler must invoke its bounder, not only the tested helper.
		{name: "oversized task intent retains Core", configure: func(f *firmwareDetailsFixture) {
			f.tasks[0].RequestedTargets.Shared.Value = strings.Repeat("<", 400000)
		}, check: func(t *testing.T, _ *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED, d.Operations.Tasks.GetError())
			require.Empty(t, d.Operations.Tasks.Entries)
			require.NotNil(t, d.Actual.GetValue())
		}},
		// A count overflow is not a successful empty queue or a reason to hide inventory.
		{name: "task count limit", configure: func(f *firmwareDetailsFixture) {
			f.store.read = func(context.Context) ([]taskdef.FirmwareTaskDetails, error) {
				return nil, &taskcommon.FirmwareTaskLimitError{RackID: f.tray.RackID}
			}
		}, check: func(t *testing.T, _ *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.NotNil(t, d.Actual.GetValue())
			require.Nil(t, d.Operations.Tasks.Entries)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED, d.Operations.Tasks.GetError())
		}},
		// A waiter survives unavailable optional context with an explicit list-read error.
		{name: "waiting context failure", configure: func(f *firmwareDetailsFixture) {
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
			f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) {
				return nil, errors.New("private-context")
			}
		}, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Equal(t, 1, f.store.contextCalls)
			require.Len(t, d.Operations.Tasks.Entries, 1)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, d.Operations.Tasks.Entries[0].WaitingContext.GetError())
		}},
		// A metadata count limit affects context alone, not the selected firmware task.
		{name: "waiting context count limit", configure: func(f *firmwareDetailsFixture) {
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
			f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) {
				return nil, &taskcommon.TaskContextLimitError{RackID: f.tray.RackID}
			}
		}, check: func(t *testing.T, _ *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Len(t, d.Operations.Tasks.Entries, 1)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED, d.Operations.Tasks.Entries[0].WaitingContext.GetError())
		}},
		// Shelf power conflicts with firmware even when it selects another component.
		{name: "firmware waiter keeps typed power conflict", configure: func(f *firmwareDetailsFixture) {
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
			f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) {
				return []taskdef.TaskContextMetadata{
					{ID: uuid.UUID{15: 4}, RackID: f.tray.RackID, Type: taskcommon.TaskTypePowerControl, Status: taskcommon.TaskStatusPending, Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypePowerShelf: {uuid.UUID{15: 5}}}}},
					{ID: uuid.UUID{15: 6}, RackID: f.tray.RackID, Type: taskcommon.TaskTypeBringUp, Status: taskcommon.TaskStatusWaiting, CreatedAt: time.Unix(150, 0)},
				}, nil
			}
		}, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Equal(t, 1, f.store.contextCalls)
			require.Len(t, d.Operations.Tasks.Entries, 1)
			waiting := d.Operations.Tasks.Entries[0].WaitingContext
			require.Nil(t, waiting.Error)
			require.Equal(t, []*pb.TaskBrief{{Id: &pb.UUID{Id: "00000000-0000-0000-0000-000000000004"}, Kind: pb.TaskKind_TASK_KIND_POWER_CONTROL, Status: pb.TaskStatus_TASK_STATUS_PENDING}}, waiting.ActiveConflicts)
			require.Equal(t, []*pb.TaskBrief{{Id: &pb.UUID{Id: "00000000-0000-0000-0000-000000000006"}, Kind: pb.TaskKind_TASK_KIND_BRING_UP, Status: pb.TaskStatus_TASK_STATUS_WAITING}}, waiting.EarlierWaiting)
		}},

		// Shelf firmware is isolated from unrelated compute power, unlike shelf
		// power. The overlapping peer and changed-status self row pin attributes/ID.
		{name: "firmware context retains its kind and recorded selection", configure: func(f *firmwareDetailsFixture) {
			f.tray.Type = devicetypes.ComponentTypePowerShelf
			f.tray.ComponentID = "ps100rt" + f.tray.ComponentID[7:]
			f.entry.ComponentId = f.tray.ComponentID
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
			f.tasks[0].Attributes = taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{f.tray.Type: {f.tray.Info.ID}}}
			f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) {
				return []taskdef.TaskContextMetadata{
					// A wrongly power-typed incoming shelf would conflict with this peer.
					{ID: uuid.UUID{15: 4}, Type: taskcommon.TaskTypePowerControl, Status: taskcommon.TaskStatusPending, Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {uuid.UUID{15: 9}}}}},
					// Recorded overlap must include this running firmware action.
					{ID: uuid.UUID{15: 5}, Type: taskcommon.TaskTypeFirmwareControl, Status: taskcommon.TaskStatusRunning, Attributes: f.tasks[0].Attributes},
					// Independent reads may see the waiter promoted; it must not block itself.
					{ID: f.tasks[0].ID, Type: taskcommon.TaskTypeFirmwareControl, Status: taskcommon.TaskStatusRunning, Attributes: f.tasks[0].Attributes},
				}, nil
			}
		}, check: func(t *testing.T, _ *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			waiting := d.Operations.Tasks.Entries[0].WaitingContext
			require.Nil(t, waiting.Error)
			require.Equal(t, []*pb.TaskBrief{{Id: &pb.UUID{Id: (uuid.UUID{15: 5}).String()}, Kind: pb.TaskKind_TASK_KIND_FIRMWARE_CONTROL, Status: pb.TaskStatus_TASK_STATUS_RUNNING}}, waiting.ActiveConflicts)
			require.Empty(t, waiting.EarlierWaiting)
		}},
		// Firmware tasks affect only their recorded selections, including shelves.
		{name: "firmware selection excludes unrelated shelf", configure: func(f *firmwareDetailsFixture) {
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
			other := f.tasks[0]
			other.ID = uuid.UUID{15: 8}
			other.Attributes = taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypePowerShelf: {uuid.UUID{15: 9}}}}
			second := f.tasks[0]
			second.ID = uuid.UUID{15: 10}
			second.CreatedAt = time.Unix(300, 0)
			f.tasks = []taskdef.FirmwareTaskDetails{other, f.tasks[0], second}
			f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) {
				return []taskdef.TaskContextMetadata{{ID: f.tasks[1].ID, RackID: f.tray.RackID, Type: taskcommon.TaskTypeFirmwareControl, Status: taskcommon.TaskStatusWaiting, CreatedAt: f.tasks[1].CreatedAt, Attributes: f.tasks[1].Attributes}}, nil
			}
		}, check: func(t *testing.T, f *firmwareDetailsFixture, d *pb.TrayFirmwareDetails) {
			require.Len(t, d.Operations.Tasks.Entries, 2)
			require.Equal(t, (uuid.UUID{15: 3}).String(), d.Operations.Tasks.Entries[0].Id.Id)
			require.Equal(t, (uuid.UUID{15: 10}).String(), d.Operations.Tasks.Entries[1].Id.Id)
			require.Empty(t, d.Operations.Tasks.Entries[0].WaitingContext.EarlierWaiting)
			require.Equal(t, []*pb.TaskBrief{{Id: &pb.UUID{Id: (uuid.UUID{15: 3}).String()}, Kind: pb.TaskKind_TASK_KIND_FIRMWARE_CONTROL, Status: pb.TaskStatus_TASK_STATUS_WAITING}}, d.Operations.Tasks.Entries[1].WaitingContext.EarlierWaiting)
			require.Equal(t, 1, f.store.contextCalls)
			require.Equal(t, []uuid.UUID{f.tray.Info.ID}, f.inventory.identityIDs)
		}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			if tc.configure != nil {
				tc.configure(f)
			}

			response, err := flowRPCClient(t, f.server).GetTrayFirmwareDetails(t.Context(), &pb.GetTrayFirmwareDetailsRequest{Id: "aa:bb:cc:dd:ee:ff"})

			require.NoError(t, err)
			require.NotNil(t, response.GetDetails())
			tc.check(t, f, response.Details)
		})
	}
	// Firmware's entry adapter owns kind rejection and sanitized inventory failures;
	// selector parsing itself remains covered by the retained resolver tests.
	for _, tc := range []struct {
		name      string
		configure func(*firmwareDetailsFixture)
		code      codes.Code
	}{
		// An inventory kind outside the three supported roles must stop both sources.
		{name: "unsupported kind", configure: func(f *firmwareDetailsFixture) { f.tray.Type = devicetypes.ComponentTypeToRSwitch }, code: codes.InvalidArgument},
		// Internal preserves its category without exposing database text.
		{name: "inventory internal failure", configure: func(f *firmwareDetailsFixture) {
			f.inventory.beforeRead = func(context.Context) error { return status.Error(codes.Internal, "private-inventory") }
		}, code: codes.Internal},
		// Other dependencies use Unavailable, with the same diagnostic redaction.
		{name: "inventory unavailable", configure: func(f *firmwareDetailsFixture) {
			f.inventory.beforeRead = func(context.Context) error { return status.Error(codes.Unavailable, "private-inventory") }
		}, code: codes.Unavailable},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			tc.configure(f)

			response, err := f.server.GetTrayFirmwareDetails(t.Context(), &pb.GetTrayFirmwareDetailsRequest{Id: f.tray.ComponentID})

			require.Nil(t, response)
			require.Equal(t, tc.code, status.Code(err))
			require.NotContains(t, err.Error(), "private-inventory")
			require.Nil(t, f.core.request)
			require.Zero(t, f.store.calls)
		})
	}
	t.Run("shared entry deadline retains a completed task read", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			started := time.Now()
			f.inventory.beforeRead = func(context.Context) error { time.Sleep(20 * time.Second); return nil }
			f.core.read = func(ctx context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) {
				<-ctx.Done()
				return nil, ctx.Err()
			}

			response, err := f.server.GetTrayFirmwareDetails(t.Context(), &pb.GetTrayFirmwareDetailsRequest{Id: f.tray.ComponentID})

			require.NoError(t, err)
			require.Equal(t, started.Add(30*time.Second), f.core.deadline)
			require.Equal(t, f.core.deadline, f.inventory.deadline)
			require.Equal(t, f.core.deadline, f.store.deadline)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED, response.Details.Actual.GetError())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED, response.Details.Desired.GetError())
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED, response.Details.Operations.Actions.GetError())
			require.Len(t, response.Details.Operations.Tasks.Entries, 1)
		})
	})
	// The reverse slow-source case protects completed Core data independently of tasks.
	t.Run("task deadline retains completed Core evidence", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			f.store.read = func(ctx context.Context) ([]taskdef.FirmwareTaskDetails, error) {
				<-ctx.Done()
				return nil, ctx.Err()
			}

			response, err := f.server.GetTrayFirmwareDetails(t.Context(), &pb.GetTrayFirmwareDetailsRequest{Id: f.tray.ComponentID})

			require.NoError(t, err)
			require.True(t, proto.Equal(f.entry.Actual, response.Details.Actual.GetValue()))
			require.True(t, proto.Equal(f.entry.Desired, response.Details.Desired.GetValue()))
			require.True(t, proto.Equal(f.entry.Actions, response.Details.Operations.Actions.GetValue()))
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED, response.Details.Operations.Tasks.GetError())
		})
	})
	// Explicit cleanup gates prove concurrent startup and joining even after the
	// task reader has advanced from primary tasks into optional waiting metadata.
	t.Run("cancellation joins Core and waiting context cleanup", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
			ctx, cancel := context.WithCancel(t.Context())
			started, canceled, completed := make(chan struct{}, 2), make(chan struct{}, 2), make(chan struct{}, 2)
			releaseCore, releaseContext := make(chan struct{}, 1), make(chan struct{}, 1)
			defer func() {
				cancel()
				close(releaseCore)
				close(releaseContext)
			}()
			f.core.read = func(ctx context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) {
				started <- struct{}{}
				<-ctx.Done()
				canceled <- struct{}{}
				<-releaseCore
				completed <- struct{}{}
				return nil, ctx.Err()
			}
			f.store.contextRead = func(ctx context.Context) ([]taskdef.TaskContextMetadata, error) {
				started <- struct{}{}
				<-ctx.Done()
				canceled <- struct{}{}
				<-releaseContext
				completed <- struct{}{}
				return nil, ctx.Err()
			}
			type result struct {
				response *pb.GetTrayFirmwareDetailsResponse
				err      error
			}
			finished := make(chan result, 1)
			go func() {
				response, err := f.server.GetTrayFirmwareDetails(ctx, &pb.GetTrayFirmwareDetailsRequest{Id: f.tray.ComponentID})
				finished <- result{response: response, err: err}
			}()

			synctest.Wait()
			require.Len(t, started, 2, "both sources must enter before cancellation")
			require.Equal(t, 1, f.store.calls)
			require.Equal(t, 1, f.store.contextCalls)
			cancel()
			synctest.Wait()
			require.Len(t, canceled, 2)
			require.Empty(t, finished)
			releaseCore <- struct{}{}
			synctest.Wait()
			require.Len(t, completed, 1)
			require.Empty(t, finished, "RPC must join waiting-context cleanup")
			releaseContext <- struct{}{}
			got := <-finished

			require.Nil(t, got.response)
			require.Equal(t, codes.Canceled, status.Code(got.err))
			require.Len(t, completed, 2)
		})
	})
}

// TestFlowServerImpl_ReadCoreFirmwareDetails pins independent acceptance gates.
// Missing evidence must not look empty, while typed Core errors and nested
// presence are forwarded intact for consumers of the Core schema to interpret.
func TestFlowServerImpl_ReadCoreFirmwareDetails(t *testing.T) {
	unsupported := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED
	invalid := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE
	tests := []struct {
		name                                    string
		configure                               func(*firmwareDetailsFixture)
		actualError, desiredError, actionsError pb.FirmwareDetailsReadError
	}{
		// Only Unimplemented establishes absent RPC dispatch support.
		{name: "older dispatch", configure: func(f *firmwareDetailsFixture) {
			f.core.read = func(context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) {
				return nil, status.Error(codes.Unimplemented, "private")
			}
		}, actualError: unsupported, desiredError: unsupported, actionsError: unsupported},
		// Authorization denial is not evidence that the method is absent.
		{name: "authorization denial", configure: func(f *firmwareDetailsFixture) {
			f.core.read = func(context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) {
				return nil, status.Error(codes.PermissionDenied, "private")
			}
		}, actualError: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, desiredError: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, actionsError: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED},
		// Evidence for another component must never be accepted.
		{name: "wrong identity", configure: func(f *firmwareDetailsFixture) { f.entry.ComponentId = "wrong-id" }, actualError: invalid, desiredError: invalid, actionsError: invalid},
		// Empty and duplicate envelopes each violate the exact-one-entry contract.
		{name: "no entries", configure: func(f *firmwareDetailsFixture) {
			f.core.read = func(context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) { return nil, nil }
		}, actualError: invalid, desiredError: invalid, actionsError: invalid},
		{name: "duplicate entries", configure: func(f *firmwareDetailsFixture) {
			f.core.read = func(context.Context) (*corev1.GetComponentFirmwareDetailsResponse, error) {
				return &corev1.GetComponentFirmwareDetailsResponse{Entries: []*corev1.ComponentFirmwareDetails{f.entry, f.entry}}, nil
			}
		}, actualError: invalid, desiredError: invalid, actionsError: invalid},
		// Every independent result gate must preserve the other available sections.
		{name: "absent actual result", configure: func(f *firmwareDetailsFixture) { f.entry.Actual = &corev1.ComponentFirmwareInstalled{} }, actualError: unsupported},
		{name: "absent desired result", configure: func(f *firmwareDetailsFixture) { f.entry.Desired = &corev1.ComponentFirmwareExpected{} }, desiredError: unsupported},
		{name: "absent stored actions", configure: func(f *firmwareDetailsFixture) { f.entry.Actions.Stored = nil }, actionsError: unsupported},
		{name: "absent backend actions", configure: func(f *firmwareDetailsFixture) { f.entry.Actions.Backend = nil }, actionsError: unsupported},
		// Typed Core failures are observations of availability, not Flow transport failures.
		{name: "typed errors remain Core values", configure: func(f *firmwareDetailsFixture) {
			failure := &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_UNSUPPORTED}
			f.entry.Actual.Result = &corev1.ComponentFirmwareInstalled_Error{Error: failure}
			f.entry.Desired.Result = &corev1.ComponentFirmwareExpected_Error{Error: failure}
		}},
		// Explicit empty evidence remains distinct from a missing outer result.
		{name: "valid empty evidence", configure: func(f *firmwareDetailsFixture) {
			f.entry.Actual.GetInventory().Entries = nil
			f.entry.Actions.Stored.Current = nil
			f.entry.Actions.Backend = &corev1.ComponentFirmwareBackendWork{}
		}},
		// Forwarding retains nested absence; downstream Core-model consumers must
		// classify the missing member result/target as unsupported, not invent data.
		{name: "nested absence remains observable", configure: func(f *firmwareDetailsFixture) {
			f.entry.Actual.GetInventory().Entries[0].Result = nil
			f.entry.Actions.Stored.Current[0].Target = nil
		}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			tc.configure(f)

			actual, desired, actions := f.server.readCoreFirmwareDetails(t.Context(), f.tray)

			if tc.actualError != 0 {
				require.Equal(t, tc.actualError, actual.GetError())
			} else {
				require.True(t, proto.Equal(f.entry.Actual, actual.GetValue()))
			}
			if tc.desiredError != 0 {
				require.Equal(t, tc.desiredError, desired.GetError())
			} else {
				require.True(t, proto.Equal(f.entry.Desired, desired.GetValue()))
			}
			if tc.actionsError != 0 {
				require.Equal(t, tc.actionsError, actions.GetError())
			} else {
				require.True(t, proto.Equal(f.entry.Actions, actions.GetValue()))
			}
		})
	}
}

// TestBoundFirmwareDetailsResponse protects optional-first degradation and the
// actual proxy JSON allowance; complete comparisons prevent losing other evidence.
func TestBoundFirmwareDetailsResponse(t *testing.T) {
	limit := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED
	huge := strings.Repeat("<", 400000) // JSON's HTML escaping expands beyond the allowance.
	tests := []struct {
		name      string
		configure func(*testing.T, *pb.GetTrayFirmwareDetailsResponse)
		expect    func(*pb.GetTrayFirmwareDetailsResponse)
		wantCode  codes.Code
	}{
		// Identity metadata must survive when removing waiting references is sufficient.
		{name: "waiting context before external identities", configure: func(t *testing.T, r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Operations.Tasks.Entries[0].WaitingContext.ActiveConflicts[0].Id.Id = huge
		}, expect: func(r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Operations.Tasks.Entries[0].WaitingContext = &pb.FirmwareTaskWaitingContext{Error: &limit}
		}},
		// After populated context gives way, oversized identities are removed alone.
		{name: "external identities", configure: func(t *testing.T, r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Operations.Tasks.Entries[0].ExternalTargets.Targets[0].Id = huge
		}, expect: func(r *pb.GetTrayFirmwareDetailsResponse) {
			task := r.Details.Operations.Tasks.Entries[0]
			task.WaitingContext = &pb.FirmwareTaskWaitingContext{Error: &limit}
			task.ExternalTargets = &pb.FirmwareTaskExternalTargets{Error: &limit}
		}},
		// Oversized primary intent fails the complete section, never a target prefix.
		{name: "primary task", configure: func(t *testing.T, r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Operations.Tasks.Entries[0].SubTargets = []string{huge}
		}, expect: func(r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Operations.Tasks = &pb.FirmwareTaskDetailsList{Error: &limit}
		}},
		// Core evidence has no lossy fallback once optional Flow data is exhausted.
		{name: "Core inventory", configure: func(t *testing.T, r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Actual = &pb.FirmwareActualDetails{Result: &pb.FirmwareActualDetails_Value{Value: &corev1.ComponentFirmwareInstalled{Result: &corev1.ComponentFirmwareInstalled_Inventory{Inventory: &corev1.ComponentFirmwareInventory{Entries: []*corev1.ComponentFirmwareInventoryEntry{{Result: &corev1.ComponentFirmwareInventoryEntry_Component{Component: &corev1.ComponentFirmwareInstalledComponent{Name: &huge}}}}}}}}}
		}, wantCode: codes.ResourceExhausted},
		// A near-boundary Core response cannot relabel an already-failed task read.
		{name: "existing task failure survives overflow", configure: func(t *testing.T, r *pb.GetTrayFirmwareDetailsResponse) {
			r.Details.Operations.Tasks = &pb.FirmwareTaskDetailsList{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED.Enum()}
			filler := ""
			r.Details.ComponentId = &filler
			message, err := protojson.Marshal(r)
			require.NoError(t, err)
			encoded, err := json.Marshal(grpcproxy.Response{ResponseJSON: message})
			require.NoError(t, err)
			filler = strings.Repeat("x", 1945*1024-len(encoded)+1)
		}, wantCode: codes.ResourceExhausted},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			task := &pb.FirmwareTaskDetails{
				Id:              &pb.UUID{Id: "kept-task"},
				Targets:         &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: "kept-target"}}}}},
				WaitingContext:  &pb.FirmwareTaskWaitingContext{ActiveConflicts: []*pb.TaskBrief{{Id: &pb.UUID{Id: "conflict"}}}},
				ExternalTargets: &pb.FirmwareTaskExternalTargets{RackId: "rack", Targets: []*pb.ExternalRef{{Id: "component"}}},
			}
			// Unpopulated and failed optional sections must remain untouched when
			// another task's populated context/identities exceed the allowance.
			empty := &pb.FirmwareTaskDetails{WaitingContext: &pb.FirmwareTaskWaitingContext{}}
			failed := &pb.FirmwareTaskDetails{WaitingContext: &pb.FirmwareTaskWaitingContext{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED.Enum()}, ExternalTargets: &pb.FirmwareTaskExternalTargets{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED.Enum()}}
			response := &pb.GetTrayFirmwareDetailsResponse{Details: &pb.TrayFirmwareDetails{Operations: &pb.FirmwareOperations{Tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{task, empty, failed, {}}}}}}
			tc.configure(t, response)
			want := proto.Clone(response).(*pb.GetTrayFirmwareDetailsResponse)
			if tc.expect != nil {
				tc.expect(want)
			}
			message, err := protojson.Marshal(response)
			require.NoError(t, err)
			encoded, err := json.Marshal(grpcproxy.Response{ResponseJSON: message})
			require.NoError(t, err)
			require.Greater(t, len(encoded), 1945*1024, "fixture must overflow the actual proxy wrapper")

			err = boundFirmwareDetailsResponse(response)

			require.Equal(t, tc.wantCode, status.Code(err))
			if err != nil {
				return
			}
			require.True(t, proto.Equal(want, response), "only the expected evidence may change")
			message, err = protojson.Marshal(response)
			require.NoError(t, err)
			encoded, err = json.Marshal(grpcproxy.Response{ResponseJSON: message})
			require.NoError(t, err)
			require.LessOrEqual(t, len(encoded), 1945*1024, "returned proxy response must fit")
		})
	}
}

// TestFlowServerImpl_AddFirmwareTaskExternalTargets proves the firmware adapter
// never substitutes Flow UUIDs, and identifies failed inventory reads without
// leaking SQL diagnostics. Shared target ordering is covered through power.
func TestFlowServerImpl_AddFirmwareTaskExternalTargets(t *testing.T) {
	tests := []struct {
		name      string
		configure func(*firmwareDetailsFixture)
		wantError pb.FirmwareDetailsReadError
		wantRack  string
		wantStage string
	}{
		// A retained rack row with no public ID must not acquire its Flow UUID.
		{name: "rack without inventory identity", configure: func(f *firmwareDetailsFixture) {
			f.tray.RackExternalID = ""
			f.inventory.rackRead = func(context.Context) ([]*rack.Rack, error) {
				return []*rack.Rack{{Info: deviceinfo.DeviceInfo{ID: f.tray.RackID}}}, nil
			}
		}, wantError: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED},
		// Rack-query errors fan out to tasks and identify the failed query stage.
		{name: "rack query failure", configure: func(f *firmwareDetailsFixture) {
			f.tray.RackExternalID = ""
			f.inventory.rackRead = func(context.Context) ([]*rack.Rack, error) {
				return nil, &pgconn.PgError{Code: "08006", Message: "private-sql", Detail: "private-value"}
			}
		}, wantError: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, wantStage: "rack_identity_read"},
		// A component-query failure retains the independently resolved rack identity.
		{name: "component query failure", configure: func(f *firmwareDetailsFixture) {
			f.inventory.identityRead = func(context.Context) ([]component.Identity, error) {
				return nil, &pgconn.PgError{Code: "08006", Message: "private-sql", Detail: "private-value"}
			}
		}, wantError: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, wantRack: "inventory-rack", wantStage: "component_identity_read"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			f := newFirmwareDetailsFixture()
			tc.configure(f)
			var diagnostics bytes.Buffer
			previous := log.Logger
			log.Logger = zerolog.New(&diagnostics)
			t.Cleanup(func() { log.Logger = previous })
			task := &pb.FirmwareTaskDetails{Id: &pb.UUID{Id: "saved-task"}, Targets: &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: f.tray.Info.ID.String()}}}}}}
			details := &pb.TrayFirmwareDetails{Operations: &pb.FirmwareOperations{Tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{task}}}}
			want := proto.Clone(task).(*pb.FirmwareTaskDetails)
			want.ExternalTargets = &pb.FirmwareTaskExternalTargets{Error: tc.wantError.Enum()}

			f.server.addFirmwareTaskExternalTargets(t.Context(), f.tray, details)

			require.Equal(t, tc.wantRack, details.GetRackExternalId())
			require.True(t, proto.Equal(want, task))
			if tc.wantStage == "" {
				require.Zero(t, f.inventory.identityCalls)
				require.Empty(t, diagnostics.String())
			} else {
				require.Contains(t, diagnostics.String(), `"failure_stage":"`+tc.wantStage+`"`)
				require.Contains(t, diagnostics.String(), `"sqlstate":"08006"`)
				require.Contains(t, diagnostics.String(), f.tray.RackID.String())
				require.NotContains(t, diagnostics.String(), "private-sql")
				require.NotContains(t, diagnostics.String(), "private-value")
			}
		})
	}
}
