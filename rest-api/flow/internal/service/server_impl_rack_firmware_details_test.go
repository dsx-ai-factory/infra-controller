// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"bytes"
	"context"
	"encoding/json"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// rackFirmwareReader records each batch and returns a separate copy of the tray
// fixture's evidence for every ID. A hook can fail, delay or corrupt one batch.
type rackFirmwareReader struct {
	entry     *corev1.ComponentFirmwareDetails
	requests  []*corev1.GetComponentFirmwareDetailsRequest
	deadlines []time.Time
	read      func(context.Context, *corev1.GetComponentFirmwareDetailsResponse) error
}

// GetComponentFirmwareDetails returns ordered independent entries, then applies
// the hook so tests can distinguish transport failure from malformed success.
func (r *rackFirmwareReader) GetComponentFirmwareDetails(ctx context.Context, req *corev1.GetComponentFirmwareDetailsRequest) (*corev1.GetComponentFirmwareDetailsResponse, error) {
	r.requests = append(r.requests, req)
	deadline, _ := ctx.Deadline()
	r.deadlines = append(r.deadlines, deadline)
	var ids []string
	for _, id := range req.GetMachineIds().GetMachineIds() {
		ids = append(ids, id.Id)
	}
	for _, id := range req.GetSwitchIds().GetIds() {
		ids = append(ids, id.Id)
	}
	for _, id := range req.GetPowerShelfIds().GetIds() {
		ids = append(ids, id.Id)
	}
	response := &corev1.GetComponentFirmwareDetailsResponse{}
	for _, id := range ids {
		entry := proto.Clone(r.entry).(*corev1.ComponentFirmwareDetails)
		entry.ComponentId = id
		entry.Actual.GetInventory().Entries[0].GetComponent().Name = proto.String(id)
		response.Entries = append(response.Entries, entry)
	}
	if r.read != nil {
		return response, r.read(ctx, response)
	}
	return response, nil
}

// rackFirmwareFixture reuses the rack inventory recorder and safe firmware
// evidence from the power and tray tests. No mutation client is present.
type rackFirmwareFixture struct {
	server    *FlowServerImpl
	inventory *rackPowerInventory
	rack      *rack.Rack
	core      *rackFirmwareReader
	store     *firmwareDetailsTaskStore
	tasks     []taskdef.FirmwareTaskDetails
	started   time.Time
}

// newRackFirmwareFixture starts with one linked host and one saved task.
func newRackFirmwareFixture() *rackFirmwareFixture {
	tray := newFirmwareDetailsFixture()
	r := &rack.Rack{Info: deviceinfo.DeviceInfo{ID: tray.tray.RackID}, ExternalID: "firmware-rack"}
	r.Components = []component.Component{*tray.tray}
	inventory := &rackPowerInventory{mockManager: newMockManager()}
	inventory.racks[r.Info.ID] = r
	f := &rackFirmwareFixture{inventory: inventory, rack: r, core: &rackFirmwareReader{entry: tray.entry}, store: tray.store, tasks: tray.tasks, started: time.Now()}
	f.store.read = func(context.Context) ([]taskdef.FirmwareTaskDetails, error) { return f.tasks, nil }
	f.server = &FlowServerImpl{inventoryManager: inventory, firmwareReader: f.core, taskStore: f.store}
	return f
}

// addMembers appends valid distinct Core IDs. Tests shuffle the complete
// inventory separately so its order cannot accidentally follow the batch order.
func (f *rackFirmwareFixture) addMembers(kind devicetypes.ComponentType, prefix string, count int) {
	for i := range count {
		f.rack.Components = append(f.rack.Components, component.Component{
			Info: deviceinfo.DeviceInfo{ID: uuid.UUID{14: byte(kind), 15: byte(i + 2)}},
			Type: kind, ComponentID: rackPowerCoreID(prefix, byte(i+2)), RackID: f.rack.Info.ID,
		})
	}
}

// TestFlowServerImpl_GetRackFirmwareDetails proves the public collection
// path. Assembly and nested backend matrices remain in their narrower tests.
func TestFlowServerImpl_GetRackFirmwareDetails(t *testing.T) {
	type testCase struct {
		name      string
		configure func(*rackFirmwareFixture)
		invoke    func(*testing.T, *rackFirmwareFixture) (*pb.GetRackFirmwareDetailsResponse, error)
		check     func(*testing.T, *rackFirmwareFixture, *pb.GetRackFirmwareDetailsResponse)
		code      codes.Code
		clock     bool
	}
	tests := []testCase{
		// The real gRPC method batches a rack larger than Core's request limit.
		{name: "public RPC batches all kinds and reads tasks once", configure: func(f *rackFirmwareFixture) {
			f.addMembers(devicetypes.ComponentTypeCompute, "fm100ht", 64)
			f.addMembers(devicetypes.ComponentTypeNVSwitch, "sw100nt", 2)
			f.addMembers(devicetypes.ComponentTypePowerShelf, "ps100rt", 2)
			f.core.read = func(_ context.Context, r *corev1.GetComponentFirmwareDetailsResponse) error {
				// Missing expected evidence must not change installed coverage.
				if len(f.core.requests) == 2 {
					r.Entries[0].Desired = nil
				}
				return nil
			}
			f.tasks[0].Status = taskcommon.TaskStatusWaiting
		}, invoke: func(t *testing.T, f *rackFirmwareFixture) (*pb.GetRackFirmwareDetailsResponse, error) {
			return flowRPCClient(t, f.server).GetRackFirmwareDetails(t.Context(), &pb.GetRackFirmwareDetailsRequest{Id: f.rack.ExternalID})
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Len(t, f.core.requests, 4)
			require.Len(t, f.core.requests[0].GetMachineIds().GetMachineIds(), 64)
			require.Len(t, f.core.requests[1].GetMachineIds().GetMachineIds(), 1)
			require.Len(t, f.core.requests[2].GetSwitchIds().GetIds(), 2)
			require.Len(t, f.core.requests[3].GetPowerShelfIds().GetIds(), 2)
			require.Len(t, r.Details.Components, 69)
			require.True(t, proto.Equal(&pb.RackFirmwareCoverage{Total: 69, Reporting: 69}, r.Details.Actual))
			require.True(t, proto.Equal(&pb.RackFirmwareCoverage{Total: 69, Reporting: 68, Unavailable: 1}, r.Details.Desired))
			ordered := slices.Clone(f.rack.Components)
			slices.SortFunc(ordered, func(a, b component.Component) int {
				if a.Type != b.Type {
					return int(a.Type) - int(b.Type)
				}
				return bytes.Compare(a.Info.ID[:], b.Info.ID[:])
			})
			var batchIDs []string
			for _, request := range f.core.requests {
				for _, id := range request.GetMachineIds().GetMachineIds() {
					batchIDs = append(batchIDs, id.Id)
				}
				for _, id := range request.GetSwitchIds().GetIds() {
					batchIDs = append(batchIDs, id.Id)
				}
				for _, id := range request.GetPowerShelfIds().GetIds() {
					batchIDs = append(batchIDs, id.Id)
				}
			}
			for i, member := range r.Details.Components {
				expected := ordered[i]
				require.Equal(t, expected.ComponentID, batchIDs[i])
				require.Equal(t, expected.Info.ID.String(), member.InternalComponentId.Id)
				require.Equal(t, expected.ComponentID, member.GetComponentId())
				require.Equal(t, pb.ComponentType(expected.Type), member.ComponentType)
				actual := proto.Clone(f.core.entry.Actual).(*corev1.ComponentFirmwareInstalled)
				actual.GetInventory().Entries[0].GetComponent().Name = proto.String(expected.ComponentID)
				require.True(t, proto.Equal(actual, member.Actual.GetValue()))
				if i == 64 {
					require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED, member.Desired.GetError())
				} else {
					require.True(t, proto.Equal(f.core.entry.Desired, member.Desired.GetValue()))
				}
				require.True(t, proto.Equal(f.core.entry.Actions, member.Operations.Actions.GetValue()))
				require.NotNil(t, member.Operations.Tasks)
				require.Nil(t, member.Operations.Tasks.Error)
				if i > 0 {
					require.Empty(t, member.Operations.Tasks.Ids)
				}
			}

			require.Equal(t, f.rack.Info.ID.String(), r.Details.RackId.Id)
			require.Equal(t, f.rack.ExternalID, r.Details.RackExternalId)
			require.Len(t, r.Details.Operations.Tasks.Entries, 1)
			require.Equal(t, 1, f.store.contextCalls)
			require.Equal(t, 1, f.inventory.identityCalls)
			require.Equal(t, f.tasks[0].ID.String(), r.Details.Components[0].Operations.Tasks.Ids[0].Id)
			for _, deadline := range f.core.deadlines {
				require.Equal(t, f.inventory.deadline, deadline)
			}
			require.False(t, f.inventory.deadline.IsZero())
			require.Equal(t, f.inventory.deadline, f.store.deadline)
			require.Equal(t, f.store.deadline, f.store.contextDeadline)
			require.Equal(t, f.rack.Info.ID, f.store.contextRackID)
			require.Equal(t, f.store.deadline, f.inventory.identityDeadline)
			require.Equal(t, []uuid.UUID{f.tasks[0].Attributes.ComponentsByType[devicetypes.ComponentTypeCompute][0]}, f.inventory.identityIDs)
			external := r.Details.Operations.Tasks.Entries[0].ExternalTargets
			require.Equal(t, "firmware-rack", external.RackId)
			require.Equal(t, powerDetailsCanonicalMachineID, external.Targets[0].Id)
			require.Equal(t, pb.ComponentType_COMPONENT_TYPE_COMPUTE, external.Targets[0].Type)
		}},
		// A failed later batch must preserve completed hosts and still read shelves.
		{name: "batch failure retains preceding and following batches", configure: func(f *rackFirmwareFixture) {
			f.addMembers(devicetypes.ComponentTypeCompute, "fm100ht", 64)
			f.addMembers(devicetypes.ComponentTypePowerShelf, "ps100rt", 1)
			f.core.read = func(context.Context, *corev1.GetComponentFirmwareDetailsResponse) error {
				if len(f.core.requests) == 2 {
					return status.Error(codes.Unavailable, "private backend diagnostic")
				}
				return nil
			}
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Len(t, f.core.requests, 3)
			require.Len(t, r.Details.Components, 66)
			require.Equal(t, uint64(65), r.Details.Actual.Reporting)
			require.Equal(t, uint64(1), r.Details.Actual.Unavailable)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, r.Details.Components[64].Actual.GetError())
			require.NotNil(t, r.Details.Components[65].Actual.GetValue())
			require.NotContains(t, r.String(), "private backend diagnostic")
		}},
		// Normalize aliases before counting the batch; invalid links stay local.
		{name: "aliases unlinked invalid and unsupported members", configure: func(f *rackFirmwareFixture) {
			member := f.rack.Components[0]
			for i, id := range []string{member.ComponentID[:7] + strings.ToUpper(member.ComponentID[7:]), "bad-id", ""} {
				alias := member
				alias.Info.ID = uuid.UUID{15: byte(i + 2)}
				alias.ComponentID = id
				f.rack.Components = append(f.rack.Components, alias)
			}
			other := member
			other.Info.ID = uuid.UUID{15: 5}
			other.Type = devicetypes.ComponentTypeToRSwitch
			f.rack.Components = append(f.rack.Components, other)
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Len(t, f.core.requests, 1)
			require.Len(t, f.core.requests[0].GetMachineIds().GetMachineIds(), 1)
			require.Len(t, r.Details.Components, 5)
			require.Equal(t, uint64(2), r.Details.Actual.Reporting)
			require.Equal(t, uint64(2), r.Details.Actual.Unavailable)
			require.Equal(t, uint64(1), r.Details.Actual.Unsupported)
			for i, code := range []pb.FirmwareDetailsReadError{
				pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED,
				pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED,
				pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED,
			} {
				member := r.Details.Components[i+2]
				require.Equal(t, code, member.Actual.GetError())
				require.Equal(t, code, member.Desired.GetError())
				require.Equal(t, code, member.Operations.Actions.GetError())
			}
			require.Nil(t, r.Details.Components[3].ComponentId)
			require.Equal(t, "bad-id", r.Details.Components[2].GetComponentId())
		}},
		// No reader means missing configuration, not a successful empty inventory.
		{name: "unconfigured Core retains tasks", configure: func(f *rackFirmwareFixture) { f.server.firmwareReader = nil }, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Empty(t, f.core.requests)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED, r.Details.Components[0].Actual.GetError())
			require.Len(t, r.Details.Operations.Tasks.Entries, 1)
		}},
		// Stored work outlives membership; a missing identity must not erase it.
		{name: "empty rack retains saved selections", configure: func(f *rackFirmwareFixture) { f.rack.Components = nil }, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Empty(t, r.Details.Components)
			require.Zero(t, r.Details.Actual.Total)
			require.Empty(t, f.core.requests)
			require.Len(t, r.Details.Operations.Tasks.Entries, 1)
			require.Len(t, r.Details.Operations.Tasks.Entries[0].Targets.Groups[0].ComponentIds, 1)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED, r.Details.Operations.Tasks.Entries[0].ExternalTargets.GetError())
		}},
		// Moved targets keep their retained external name, without becoming current members.
		{name: "moved selection keeps external identity", configure: func(f *rackFirmwareFixture) {
			moved := f.rack.Components[0]
			moved.ComponentID = "retained-external-name"
			otherID := uuid.UUID{15: 99}
			f.inventory.racks[otherID] = &rack.Rack{Components: []component.Component{moved}}
			f.rack.Components = nil
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Empty(t, r.Details.Components)
			external := r.Details.Operations.Tasks.Entries[0].ExternalTargets
			require.Nil(t, external.Error)
			require.Equal(t, "firmware-rack", external.RackId)
			require.Len(t, external.Targets, 1)
			require.Equal(t, "retained-external-name", external.Targets[0].Id)
			require.Equal(t, pb.ComponentType_COMPONENT_TYPE_COMPUTE, external.Targets[0].Type)
		}},
		// A rack task failure must propagate to references without hiding Core facts.
		{name: "failed tasks retain component evidence", configure: func(f *rackFirmwareFixture) {
			f.store.read = func(context.Context) ([]taskdef.FirmwareTaskDetails, error) {
				return nil, status.Error(codes.Unavailable, "private task diagnostic")
			}
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Equal(t, uint64(1), r.Details.Actual.Reporting)
			require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED, r.Details.Operations.Tasks.GetError())
			require.Equal(t, r.Details.Operations.Tasks.Error, r.Details.Components[0].Operations.Tasks.Error)
			require.Empty(t, r.Details.Components[0].Operations.Tasks.Ids)
		}},
		// The second batch consumes the shared deadline; subsequent kinds never dispatch.
		{name: "expiry preserves completed batch and tasks", clock: true, configure: func(f *rackFirmwareFixture) {
			f.addMembers(devicetypes.ComponentTypeCompute, "fm100ht", 64)
			f.addMembers(devicetypes.ComponentTypePowerShelf, "ps100rt", 1)
			f.inventory.beforeRead = func(context.Context) error { time.Sleep(7 * time.Second); return nil }
			f.core.read = func(ctx context.Context, _ *corev1.GetComponentFirmwareDetailsResponse) error {
				if len(f.core.requests) == 2 {
					<-ctx.Done()
					return nil
				}
				return nil
			}
		}, invoke: func(t *testing.T, f *rackFirmwareFixture) (*pb.GetRackFirmwareDetailsResponse, error) {
			ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
			defer cancel()
			return f.server.GetRackFirmwareDetails(ctx, &pb.GetRackFirmwareDetailsRequest{Id: f.rack.ExternalID})
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Len(t, f.core.requests, 2)
			require.Len(t, r.Details.Components, 66)
			require.Equal(t, uint64(64), r.Details.Actual.Reporting)
			require.Equal(t, uint64(2), r.Details.Actual.Unavailable)
			for _, member := range r.Details.Components[64:] {
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED, member.Actual.GetError())
			}
			require.Len(t, r.Details.Operations.Tasks.Entries, 1)
			require.Equal(t, 9*time.Second, f.core.deadlines[0].Sub(f.started))
			require.Equal(t, f.inventory.deadline, f.core.deadlines[0])
			require.Equal(t, f.inventory.deadline, f.store.deadline)
			require.Equal(t, f.inventory.deadline, f.inventory.identityDeadline)
		}},
		// A full rack of waiters can overflow through context alone. All primary
		// records, selections and per-component references must remain present.
		{name: "waiting context yields to complete primary tasks", configure: func(f *rackFirmwareFixture) {
			template := f.tasks[0]
			f.tasks = nil
			var metadata []taskdef.TaskContextMetadata
			for i := range 256 {
				task := template
				task.ID = uuid.UUID{14: 2, 15: byte(i)}
				task.Status = taskcommon.TaskStatusWaiting
				task.CreatedAt = task.CreatedAt.Add(time.Duration(i) * time.Second)
				f.tasks = append(f.tasks, task)
				metadata = append(metadata, taskdef.TaskContextMetadata{ID: task.ID, RackID: task.RackID, Type: taskcommon.TaskTypeFirmwareControl, Status: task.Status, Attributes: task.Attributes, CreatedAt: task.CreatedAt})
			}
			f.store.contextRead = func(context.Context) ([]taskdef.TaskContextMetadata, error) { return metadata, nil }
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Equal(t, 1, f.store.contextCalls)
			require.Equal(t, 1, f.inventory.identityCalls)
			require.Equal(t, []uuid.UUID{uuid.UUID{15: 1}}, f.inventory.identityIDs)
			require.Len(t, r.Details.Operations.Tasks.Entries, 256)
			require.Len(t, r.Details.Components[0].Operations.Tasks.Ids, 256)
			require.Nil(t, r.Details.Operations.Tasks.Entries[0].WaitingContext.Error)
			for i, task := range r.Details.Operations.Tasks.Entries {
				require.Equal(t, f.tasks[i].ID.String(), task.Id.Id)
				require.Equal(t, task.Id.Id, r.Details.Components[0].Operations.Tasks.Ids[i].Id)
				require.Len(t, task.Targets.Groups[0].ComponentIds, 1)
				require.Len(t, task.ExternalTargets.Targets, 1)
				if i > 0 {
					require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED, task.WaitingContext.GetError())
				}
			}
			fits, err := detailsResponseFits(r)
			require.NoError(t, err)
			require.True(t, fits)
		}},
		// Complete primary intent cannot be silently reduced to fit the proxy.
		{name: "primary task too large", code: codes.ResourceExhausted, configure: func(f *rackFirmwareFixture) {
			f.tasks[0].RequestedTargets.Shared.Value = strings.Repeat("x", 1945*1024)
		}},
		// Hardware evidence is also bounded; rejecting the RPC preserves membership semantics.
		{name: "inventory too large", code: codes.ResourceExhausted, configure: func(f *rackFirmwareFixture) {
			f.core.entry.Actual.GetInventory().Entries[0].GetComponent().Version = proto.String(strings.Repeat("x", 1945*1024))
		}},
	}
	// Each malformed response is rejected as a whole before member association.
	for _, malformed := range []struct {
		name  string
		alter func(*corev1.GetComponentFirmwareDetailsResponse)
	}{
		{"missing entry", func(r *corev1.GetComponentFirmwareDetailsResponse) { r.Entries = r.Entries[:1] }},
		{"surplus entry", func(r *corev1.GetComponentFirmwareDetailsResponse) { r.Entries = append(r.Entries, r.Entries[0]) }},
		{"reordered entries", func(r *corev1.GetComponentFirmwareDetailsResponse) { slices.Reverse(r.Entries) }},
		{"wrong component kind", func(r *corev1.GetComponentFirmwareDetailsResponse) {
			// Leave the first entry valid to prove validation reaches later positions.
			r.Entries[1].ComponentId = "sw100nt" + r.Entries[1].ComponentId[7:]
		}},
	} {
		tests = append(tests, testCase{name: malformed.name, configure: func(f *rackFirmwareFixture) {
			f.addMembers(devicetypes.ComponentTypeCompute, "fm100ht", 1)
			f.addMembers(devicetypes.ComponentTypePowerShelf, "ps100rt", 1)
			f.core.read = func(_ context.Context, r *corev1.GetComponentFirmwareDetailsResponse) error {
				if len(f.core.requests) == 1 {
					malformed.alter(r)
				}
				return nil
			}
		}, check: func(t *testing.T, f *rackFirmwareFixture, r *pb.GetRackFirmwareDetailsResponse) {
			require.Len(t, r.Details.Components, 3)
			require.Equal(t, uint64(1), r.Details.Actual.Reporting)
			for _, member := range r.Details.Components[:2] {
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE, member.Actual.GetError())
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE, member.Desired.GetError())
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE, member.Operations.Actions.GetError())
			}
			require.NotNil(t, r.Details.Components[2].Actual.GetValue())
		}})
	}
	// Cancellation is complete only after both dependency branches exit, in
	// either cleanup order. Virtual time keeps these gates independent of sleeps.
	for _, coreFirst := range []bool{true, false} {
		name := "cancellation joins Core last"
		if coreFirst {
			name = "cancellation joins tasks last"
		}
		tests = append(tests, testCase{name: name, code: codes.Canceled, clock: true,
			configure: func(f *rackFirmwareFixture) { f.addMembers(devicetypes.ComponentTypePowerShelf, "ps100rt", 1) },
			invoke: func(t *testing.T, f *rackFirmwareFixture) (*pb.GetRackFirmwareDetailsResponse, error) {
				ctx, cancel := context.WithCancel(t.Context())
				canceled := make(chan error, 2)
				started, stopped := make(chan struct{}, 2), make(chan struct{}, 2)
				releaseCore, releaseTasks := make(chan struct{}), make(chan struct{})
				defer cancel()
				defer close(releaseCore)
				defer close(releaseTasks)
				f.core.read = func(ctx context.Context, _ *corev1.GetComponentFirmwareDetailsResponse) error {
					started <- struct{}{}
					<-ctx.Done()
					canceled <- ctx.Err()
					<-releaseCore
					stopped <- struct{}{}
					return ctx.Err()
				}
				f.store.read = func(ctx context.Context) ([]taskdef.FirmwareTaskDetails, error) {
					started <- struct{}{}
					<-ctx.Done()
					canceled <- ctx.Err()
					<-releaseTasks
					stopped <- struct{}{}
					return nil, ctx.Err()
				}
				type result struct {
					response *pb.GetRackFirmwareDetailsResponse
					err      error
				}
				returned := make(chan result, 1)
				go func() {
					response, err := f.server.GetRackFirmwareDetails(ctx, &pb.GetRackFirmwareDetailsRequest{Id: f.rack.ExternalID})
					returned <- result{response, err}
				}()

				synctest.Wait()
				require.Len(t, started, 2)
				cancel()
				synctest.Wait()
				require.Len(t, canceled, 2, "both readers observe cancellation before cleanup is released")
				require.ErrorIs(t, <-canceled, context.Canceled)
				require.ErrorIs(t, <-canceled, context.Canceled)
				first, second := releaseCore, releaseTasks
				if !coreFirst {
					first, second = second, first
				}
				first <- struct{}{}
				synctest.Wait()
				require.Len(t, stopped, 1)
				require.Empty(t, returned, "the remaining reader still owns cleanup")
				second <- struct{}{}
				got := <-returned

				require.Len(t, stopped, 2)
				require.Len(t, f.core.requests, 1, "cancellation prevents dispatch of the shelf batch")
				return got.response, got.err
			},
		})
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			run := func(t *testing.T) {
				f := newRackFirmwareFixture()
				if test.configure != nil {
					test.configure(f)
				}
				slices.Reverse(f.rack.Components)
				original := slices.Clone(f.rack.Components)
				var diagnostics bytes.Buffer
				previousLogger := log.Logger
				log.Logger = zerolog.New(&diagnostics)
				defer func() { log.Logger = previousLogger }()

				var response *pb.GetRackFirmwareDetailsResponse
				var err error
				if test.invoke != nil {
					response, err = test.invoke(t, f)
				} else {
					response, err = f.server.GetRackFirmwareDetails(t.Context(), &pb.GetRackFirmwareDetailsRequest{Id: f.rack.ExternalID})
				}

				require.Equal(t, test.code, status.Code(err), "%v", err)
				require.NotContains(t, diagnostics.String(), "private")
				if test.name == "batch failure retains preceding and following batches" {
					require.Contains(t, diagnostics.String(), `"grpc_status_code":"Unavailable"`)
					require.Contains(t, diagnostics.String(), f.rack.Info.ID.String())
				}
				if !slices.ContainsFunc(f.tasks, func(task taskdef.FirmwareTaskDetails) bool { return task.Status == taskcommon.TaskStatusWaiting }) {
					require.Zero(t, f.store.contextCalls)
				}
				require.Equal(t, original, f.rack.Components, "inventory remains caller-owned")
				require.Equal(t, 1, f.inventory.calls)
				require.True(t, f.inventory.withComponents)
				require.Equal(t, 1, f.store.calls)
				require.Equal(t, f.rack.Info.ID, f.store.rackID)
				if test.code != codes.OK {
					require.Nil(t, response)
				} else {
					require.NotNil(t, response)
				}
				if test.check != nil {
					test.check(t, f, response)
				}
			}
			if test.clock {
				synctest.Test(t, run)
			} else {
				run(t)
			}
		})
	}
	// Inventory errors abort before downstream calls and never echo private text.
	for _, test := range []struct {
		name, id                           string
		failure, want                      codes.Code
		nilRack, expire, cancelAfterExpiry bool
		lookups                            int
	}{
		{name: "missing selector", want: codes.InvalidArgument},
		{name: "unknown selector", id: "missing", failure: codes.NotFound, want: codes.NotFound, lookups: 1},
		{name: "internal UUID is not selector", id: uuid.UUID{15: 2}.String(), want: codes.NotFound, lookups: 1},
		{name: "inventory unavailable", id: "firmware-rack", failure: codes.Unknown, want: codes.Unavailable, lookups: 1},
		{name: "inventory internal error", id: "firmware-rack", failure: codes.Internal, want: codes.Internal, lookups: 1},
		{name: "nil inventory", id: "firmware-rack", nilRack: true, want: codes.Internal, lookups: 1},
		// Parent cancellation during timed-out inventory cleanup takes precedence.
		{name: "cancel during expired inventory cleanup", id: "firmware-rack", expire: true, cancelAfterExpiry: true, want: codes.Canceled, lookups: 1},
		{name: "inventory deadline", id: "firmware-rack", expire: true, want: codes.DeadlineExceeded, lookups: 1},
	} {
		t.Run(test.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f := newRackFirmwareFixture()
				f.inventory.nilRack = test.nilRack
				ctx, cancel := context.WithCancel(t.Context())
				defer cancel()
				var diagnostics bytes.Buffer
				previousLogger := log.Logger
				log.Logger = zerolog.New(&diagnostics)
				defer func() { log.Logger = previousLogger }()
				f.inventory.beforeRead = func(ctx context.Context) error {
					if test.expire {
						<-ctx.Done()
						if test.cancelAfterExpiry {
							cancel()
						}
						return ctx.Err()
					}
					if test.failure != codes.OK {
						return status.Error(test.failure, "private inventory diagnostic")
					}
					return nil
				}

				response, err := f.server.GetRackFirmwareDetails(ctx, &pb.GetRackFirmwareDetailsRequest{Id: test.id})

				require.Nil(t, response)
				require.Equal(t, test.want, status.Code(err))
				require.Equal(t, test.lookups, f.inventory.calls)
				require.Zero(t, f.store.calls)
				require.Empty(t, f.core.requests)
				require.NotContains(t, err.Error(), "private inventory diagnostic")
				require.NotContains(t, diagnostics.String(), "private inventory diagnostic")
				if test.failure == codes.Unknown || test.failure == codes.Internal {
					require.Contains(t, diagnostics.String(), `"grpc_status_code":"`+test.failure.String()+`"`)
				}
			})
		})
	}
}

// TestBoundRackFirmwareDetailsResponse protects the rack's complete primary
// evidence: only optional waiting context may yield to the encoded proxy limit.
func TestBoundRackFirmwareDetailsResponse(t *testing.T) {
	for _, test := range []struct {
		name              string
		waiting, identity string
		code              codes.Code
	}{
		// A populated context must survive when the complete response fits.
		{name: "fitting context", waiting: "earlier-task", identity: "retained-target"},
		// Inner protobuf JSON fits, but the real proxy wrapper escapes '<'.
		{name: "wrapper overflow removes only context", waiting: strings.Repeat("<", 400000), identity: "retained-target"},
		// Dropping context cannot justify discarding oversized retained identities.
		{name: "identities still exceed the limit", waiting: "earlier-task", identity: strings.Repeat("<", 400000), code: codes.ResourceExhausted},
	} {
		t.Run(test.name, func(t *testing.T) {
			task := &pb.FirmwareTaskDetails{
				Id:              &pb.UUID{Id: "primary-task"},
				Targets:         &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: "stored-selection"}}}}},
				ExternalTargets: &pb.FirmwareTaskExternalTargets{RackId: "rack", Targets: []*pb.ExternalRef{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, Id: test.identity}}},
				WaitingContext:  &pb.FirmwareTaskWaitingContext{EarlierWaiting: []*pb.TaskBrief{{Id: &pb.UUID{Id: test.waiting}}}},
			}
			response := &pb.GetRackFirmwareDetailsResponse{Details: &pb.RackFirmwareDetails{
				RackExternalId: "rack", Operations: &pb.RackFirmwareOperations{Tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{task}}},
				Components: []*pb.RackComponentFirmwareDetails{{ComponentId: proto.String("hardware"), Operations: &pb.RackComponentFirmwareOperations{Tasks: &pb.FirmwareTaskReferenceList{Ids: []*pb.UUID{{Id: "primary-task"}}}}}},
			}}
			want := proto.Clone(response).(*pb.GetRackFirmwareDetailsResponse)
			message, err := protojson.Marshal(response)
			require.NoError(t, err)
			require.Less(t, len(message), 1945*1024, "only the proxy encoding crosses the limit")
			encoded, err := json.Marshal(grpcproxy.Response{ResponseJSON: message})
			require.NoError(t, err)
			if test.name != "fitting context" {
				require.Greater(t, len(encoded), 1945*1024)
				want.Details.Operations.Tasks.Entries[0].WaitingContext = &pb.FirmwareTaskWaitingContext{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum()}
			}

			err = boundRackFirmwareDetailsResponse(response)

			require.Equal(t, test.code, status.Code(err))
			require.True(t, proto.Equal(want, response), "every primary field survives unchanged")
			require.Equal(t, test.waiting, task.WaitingContext.EarlierWaiting[0].Id.Id, "bounding does not mutate shared input")
		})
	}
}
