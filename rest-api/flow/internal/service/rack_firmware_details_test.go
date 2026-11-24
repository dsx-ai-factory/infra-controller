// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"slices"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// TestAssembleRackFirmwareDetails protects complete membership and recorded task
// selection without repeating the tray reader's transport and backend tests.
func TestAssembleRackFirmwareDetails(t *testing.T) {
	// newFirmwareDetailsFixture supplies distinct installed/expected/requested
	// versions and stored actions beside a backend PollFailed error. No fake is called.
	fixture := newFirmwareDetailsFixture()
	sections := firmwareCoreDetails{
		actual:  &pb.FirmwareActualDetails{Result: &pb.FirmwareActualDetails_Value{Value: fixture.entry.Actual}},
		desired: &pb.FirmwareDesiredDetails{Result: &pb.FirmwareDesiredDetails_Value{Value: fixture.entry.Desired}},
		actions: &pb.FirmwareActionDetails{Result: &pb.FirmwareActionDetails_Value{Value: fixture.entry.Actions}},
	}
	// Repeated physical inventory entries and a failed peer must survive intact;
	// a rack cannot collapse them into one version or discard the healthy rows.
	inventory := sections.actual.GetValue().GetInventory()
	inventory.Entries = append(inventory.Entries, inventory.Entries[0], &corev1.ComponentFirmwareInventoryEntry{
		Result: &corev1.ComponentFirmwareInventoryEntry_Error{Error: &corev1.ComponentFirmwareError{
			Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_READ_FAILED,
		}},
	})
	sections.desired.GetValue().GetHost().Entries = append(sections.desired.GetValue().GetHost().Entries,
		&corev1.ComponentHostFirmwareExpectation{Component: "BMC", Version: "different-component-version"})
	compute := *fixture.tray
	switchMember := component.Component{Type: devicetypes.ComponentTypeNVSwitch, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 9}}, ComponentID: "switch"}
	shelf := component.Component{Type: devicetypes.ComponentTypePowerShelf, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 3}}, ComponentID: "shelf"}
	shared, err := firmwareTaskDetailsToProto(fixture.tasks[0])
	require.NoError(t, err)
	shared.Id = protobuf.UUIDTo(uuid.UUID{14: 1})
	shared.Targets = protobuf.ComponentsByTypeTo(map[devicetypes.ComponentType][]uuid.UUID{
		compute.Type: {compute.Info.ID, compute.Info.ID}, switchMember.Type: {switchMember.Info.ID},
	})
	shared.ExternalTargets = &pb.FirmwareTaskExternalTargets{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED.Enum()}
	// A shelf selection never implies compute membership, even when the saved
	// UUID happens to match under a different type.
	shelfTask := proto.Clone(shared).(*pb.FirmwareTaskDetails)
	shelfTask.Id = protobuf.UUIDTo(uuid.UUID{14: 2})
	shelfTask.Targets = protobuf.ComponentsByTypeTo(map[devicetypes.ComponentType][]uuid.UUID{shelf.Type: {shelf.Info.ID, compute.Info.ID}})
	removedTask := proto.Clone(shared).(*pb.FirmwareTaskDetails)
	removedTask.Id = protobuf.UUIDTo(uuid.UUID{14: 3})
	removedTask.CreatedAt = timestamppb.New(time.Unix(199, 0))
	removedTask.Targets = protobuf.ComponentsByTypeTo(map[devicetypes.ComponentType][]uuid.UUID{compute.Type: {{15: 99}, compute.Info.ID}})

	// Crossing sort keys distinguish creation time from task UUID, and type from
	// member UUID. Different statuses and identity outcomes remain task-local.
	shelfTask.Status = pb.TaskStatus_TASK_STATUS_RUNNING
	shelfTask.ExternalTargets = &pb.FirmwareTaskExternalTargets{RackId: "inventory-rack", Targets: []*pb.ExternalRef{
		{Type: protobuf.ComponentTypeTo(shelf.Type), Id: shelf.ComponentID},
		{Type: protobuf.ComponentTypeTo(shelf.Type), Id: "other-shelf"},
	}}
	removedTask.Status = pb.TaskStatus_TASK_STATUS_WAITING
	removedTask.WaitingContext = &pb.FirmwareTaskWaitingContext{ActiveConflicts: []*pb.TaskBrief{{
		Id: protobuf.UUIDTo(uuid.UUID{14: 9}), Kind: pb.TaskKind_TASK_KIND_POWER_CONTROL, Status: pb.TaskStatus_TASK_STATUS_RUNNING,
	}}}
	unlinkedTask := proto.Clone(shared).(*pb.FirmwareTaskDetails)
	unlinkedTask.Targets = protobuf.ComponentsByTypeTo(map[devicetypes.ComponentType][]uuid.UUID{
		compute.Type: {{15: 4}}, devicetypes.ComponentTypeUMS: {{15: 7}},
	})

	// Each UUID has different evidence. A failed installed read can coexist with
	// full RMS comparisons, including repeated mismatch rows, and an action error.
	comparison := &corev1.ComponentFirmwareComparison{Component: proto.String("switch-firmware"), ProposedVersion: proto.String("manifest-version"), Status: corev1.ComponentFirmwareComparisonStatus_COMPONENT_FIRMWARE_COMPARISON_STATUS_MISMATCHED}
	switchSections := firmwareCoreDetails{
		actual: &pb.FirmwareActualDetails{Result: &pb.FirmwareActualDetails_Value{Value: &corev1.ComponentFirmwareInstalled{
			Result: &corev1.ComponentFirmwareInstalled_Error{Error: &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_READ_FAILED}},
		}}},
		desired: &pb.FirmwareDesiredDetails{Result: &pb.FirmwareDesiredDetails_Value{Value: &corev1.ComponentFirmwareExpected{
			Result: &corev1.ComponentFirmwareExpected_Comparisons{Comparisons: &corev1.ComponentFirmwareComparisons{Entries: []*corev1.ComponentFirmwareComparison{comparison, comparison}}},
		}}},
		actions: &pb.FirmwareActionDetails{Result: &pb.FirmwareActionDetails_Error{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_DEADLINE_EXCEEDED}},
	}
	shelfSections := firmwareCoreDetails{
		actual: &pb.FirmwareActualDetails{Result: &pb.FirmwareActualDetails_Value{Value: &corev1.ComponentFirmwareInstalled{
			Result: &corev1.ComponentFirmwareInstalled_Error{Error: &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_UNSUPPORTED}},
		}}},
		desired: &pb.FirmwareDesiredDetails{Result: &pb.FirmwareDesiredDetails_Error{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_CONFIGURED}},
		actions: &pb.FirmwareActionDetails{Result: &pb.FirmwareActionDetails_Value{Value: &corev1.ComponentFirmwareActions{Stored: &corev1.ComponentStoredFirmwareActions{}, Backend: &corev1.ComponentFirmwareBackendWork{}}}},
	}
	emptyHost := &pb.FirmwareDesiredDetails{Result: &pb.FirmwareDesiredDetails_Value{Value: &corev1.ComponentFirmwareExpected{
		Result: &corev1.ComponentFirmwareExpected_Host{Host: &corev1.ComponentHostFirmwareExpectations{}},
	}}}
	invalidGroup := proto.Clone(shared).(*pb.FirmwareTaskDetails)
	invalidGroup.Targets.Groups = append(invalidGroup.Targets.Groups, nil)

	for _, test := range []struct {
		name    string
		members []component.Component
		core    map[uuid.UUID]firmwareCoreDetails
		tasks   *pb.FirmwareTaskDetailsList
		check   func(*testing.T, rackFirmwareDetails)
	}{
		{
			// Empty inventory and a successful empty task read are real results.
			name: "empty rack", tasks: &pb.FirmwareTaskDetailsList{},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Empty(t, got.components)
				require.Equal(t, rackFirmwareCoverage{}, got.actual)
				require.Equal(t, rackFirmwareCoverage{}, got.desired)
				require.Nil(t, got.tasks.Error)
				require.Empty(t, got.tasks.Entries)
			},
		},
		{
			// Full collections, independent action errors, exact typed selection,
			// and removed targets all survive the same rack composition.
			name:    "heterogeneous members and shared tasks",
			members: []component.Component{shelf, switchMember, compute},
			core: map[uuid.UUID]firmwareCoreDetails{
				compute.Info.ID: sections, switchMember.Info.ID: switchSections, shelf.Info.ID: shelfSections,
			},
			tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{shelfTask, removedTask, shared, proto.Clone(shared).(*pb.FirmwareTaskDetails)}},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Len(t, got.components, 3)
				for i, want := range []component.Component{compute, switchMember, shelf} {
					member := got.components[i]
					require.Equal(t, want.Info.ID, member.internalComponentID)
					require.Equal(t, want.ComponentID, member.componentID)
					require.Equal(t, want.Type, member.componentType)
					wantSections := []firmwareCoreDetails{sections, switchSections, shelfSections}[i]
					require.True(t, proto.Equal(wantSections.actual, member.actual))
					require.True(t, proto.Equal(wantSections.desired, member.desired))
					require.True(t, proto.Equal(wantSections.actions, member.actions))
					require.Nil(t, member.tasksError)
				}
				require.Equal(t, []uuid.UUID{{14: 3}, {14: 1}}, got.components[0].taskIDs)
				require.Equal(t, []uuid.UUID{{14: 1}}, got.components[1].taskIDs)
				require.Equal(t, []uuid.UUID{{14: 2}}, got.components[2].taskIDs)
				require.Len(t, got.tasks.Entries, 3)
				for i, want := range []*pb.FirmwareTaskDetails{removedTask, shared, shelfTask} {
					require.True(t, proto.Equal(want, got.tasks.Entries[i]), "complete task %d", i)
				}
				require.Equal(t, rackFirmwareCoverage{total: 3, reporting: 1, unsupported: 1, unavailable: 1}, got.actual)
				require.Equal(t, rackFirmwareCoverage{total: 3, reporting: 2, unavailable: 1}, got.desired)
			},
		},
		{
			// Two empty external IDs must remain separate. Missing reads and an
			// unsupported inventory kind must also remain visible in coverage.
			name: "unlinked missing and unsupported members",
			members: []component.Component{
				{Type: devicetypes.ComponentTypeUMS, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 7}}},
				{Type: compute.Type, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 6}}, ComponentID: "linked-without-result"},
				{Type: compute.Type, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 5}}},
				{Type: compute.Type, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 4}}},
			},
			// Supplied Core data cannot override absent linkage or passive support.
			core:  map[uuid.UUID]firmwareCoreDetails{{15: 4}: sections, {15: 7}: sections},
			tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{unlinkedTask}},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Len(t, got.components, 4)
				for i, code := range []pb.FirmwareDetailsReadError{
					pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED,
					pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED,
					pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED,
					pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED,
				} {
					member := got.components[i]
					require.Equal(t, uuid.UUID{15: byte(i + 4)}, member.internalComponentID)
					require.Equal(t, code, member.actual.GetError())
					require.Equal(t, code, member.desired.GetError())
					require.Equal(t, code, member.actions.GetError())
					require.Equal(t, []string{"", "", "linked-without-result", ""}[i], member.componentID)
					require.Nil(t, member.tasksError)
				}
				// Recorded selection remains readable without hardware support/linkage.
				require.Equal(t, []uuid.UUID{{14: 1}}, got.components[0].taskIDs)
				require.Empty(t, got.components[1].taskIDs)
				require.Empty(t, got.components[2].taskIDs)
				require.Equal(t, []uuid.UUID{{14: 1}}, got.components[3].taskIDs)
				require.Len(t, got.tasks.Entries, 1)
				require.True(t, proto.Equal(unlinkedTask, got.tasks.Entries[0]))
				require.Equal(t, rackFirmwareCoverage{total: 4, unsupported: 1, unavailable: 3}, got.actual)
				require.Equal(t, got.actual, got.desired)
			},
		},
		{
			// A failed task query cannot erase a successful Core section, and an
			// absent Core section cannot be mistaken for a successful empty one.
			name:    "independent missing sections and task failure",
			members: []component.Component{compute, {Type: compute.Type, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 6}}, ComponentID: "other-host"}, {Type: devicetypes.ComponentTypeUMS, Info: deviceinfo.DeviceInfo{ID: uuid.UUID{15: 7}}}},
			core: map[uuid.UUID]firmwareCoreDetails{
				compute.Info.ID: {actual: sections.actual, actions: sections.actions},
				{15: 6}:         {desired: emptyHost},
			},
			// Even an inconsistent failed list cannot publish partial task references.
			tasks: &pb.FirmwareTaskDetailsList{Error: pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum(), Entries: []*pb.FirmwareTaskDetails{shared}},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Len(t, got.components, 3)
				member := got.components[0]
				require.True(t, proto.Equal(sections.actual, member.actual))
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED, member.desired.GetError())
				require.True(t, proto.Equal(sections.actions, member.actions))
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED, got.components[1].actual.GetError())
				require.True(t, proto.Equal(emptyHost, got.components[1].desired))
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED, got.components[1].actions.GetError())
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum(), got.tasks.Error)
				for _, member := range got.components {
					require.Equal(t, got.tasks.Error, member.tasksError)
					require.Empty(t, member.taskIDs)
				}
				require.Empty(t, got.tasks.Entries)
				require.Equal(t, rackFirmwareCoverage{total: 3, reporting: 1, unsupported: 1, unavailable: 1}, got.actual)
				require.Equal(t, rackFirmwareCoverage{total: 3, reporting: 1, unsupported: 1, unavailable: 1}, got.desired)
			},
		},
		{
			// A removed selection remains rack work even with no current members.
			name: "empty inventory with saved work", tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{removedTask}},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Empty(t, got.components)
				require.Nil(t, got.tasks.Error)
				require.Len(t, got.tasks.Entries, 1)
				require.True(t, proto.Equal(removedTask, got.tasks.Entries[0]))
			},
		},
		{
			// Missing task evidence differs from an explicitly successful empty read.
			name: "missing task list", members: []component.Component{compute},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED.Enum(), got.tasks.Error)
				require.Equal(t, got.tasks.Error, got.components[0].tasksError)
				require.Empty(t, got.tasks.Entries)
				require.Empty(t, got.components[0].taskIDs)
			},
		},
		{
			// A missing ID must fail the whole list, including earlier valid tasks.
			name: "invalid task identity", members: []component.Component{compute}, tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{shared, {}}},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum(), got.tasks.Error)
				require.Equal(t, got.tasks.Error, got.components[0].tasksError)
				require.Empty(t, got.tasks.Entries)
				require.Empty(t, got.components[0].taskIDs)
			},
		},
		{
			// A broken target group cannot turn into an apparently empty selection.
			name: "invalid target group", members: []component.Component{compute}, tasks: &pb.FirmwareTaskDetailsList{Entries: []*pb.FirmwareTaskDetails{invalidGroup}},
			check: func(t *testing.T, got rackFirmwareDetails) {
				require.Equal(t, pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum(), got.tasks.Error)
				require.Empty(t, got.tasks.Entries)
				require.Empty(t, got.components[0].taskIDs)
			},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			originalMembers := slices.Clone(test.members)
			originalTasks := proto.Clone(test.tasks)
			originalCore := make(map[uuid.UUID]*pb.TrayFirmwareDetails, len(test.core))
			for id, sections := range test.core {
				originalCore[id] = proto.Clone(&pb.TrayFirmwareDetails{Actual: sections.actual, Desired: sections.desired, Operations: &pb.FirmwareOperations{Actions: sections.actions}}).(*pb.TrayFirmwareDetails)
			}

			got := assembleRackFirmwareDetails(test.members, test.core, test.tasks)

			test.check(t, got)
			require.Equal(t, originalMembers, test.members, "inventory order belongs to the caller")
			require.True(t, proto.Equal(originalTasks, test.tasks), "task sorting and deduplication must not alter input")
			require.Len(t, test.core, len(originalCore))
			for id, original := range originalCore {
				sections, found := test.core[id]
				require.True(t, found)
				require.True(t, proto.Equal(original.Actual, sections.actual), "installed input for %s", id)
				require.True(t, proto.Equal(original.Desired, sections.desired), "expected input for %s", id)
				require.True(t, proto.Equal(original.Operations.Actions, sections.actions), "actions input for %s", id)
			}
		})
	}
}

// TestSummarizeRackFirmware distinguishes received collections from explicit
// backend non-support and missing evidence, independently for each section.
func TestSummarizeRackFirmware(t *testing.T) {
	for _, test := range []struct {
		name    string
		actual  *corev1.ComponentFirmwareInstalled
		desired *corev1.ComponentFirmwareExpected
		want    [2]rackFirmwareCoverage
	}{
		{
			// A successful empty collection reports, including empty RMS comparisons.
			name:    "empty collections",
			actual:  &corev1.ComponentFirmwareInstalled{Result: &corev1.ComponentFirmwareInstalled_Inventory{Inventory: &corev1.ComponentFirmwareInventory{}}},
			desired: &corev1.ComponentFirmwareExpected{Result: &corev1.ComponentFirmwareExpected_Comparisons{Comparisons: &corev1.ComponentFirmwareComparisons{}}},
			want:    [2]rackFirmwareCoverage{{total: 1, reporting: 1}, {total: 1, reporting: 1}},
		},
		{
			// An installed capability failure does not hide a separate desired error.
			name:    "backend unsupported and unavailable",
			actual:  &corev1.ComponentFirmwareInstalled{Result: &corev1.ComponentFirmwareInstalled_Error{Error: &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_UNSUPPORTED}}},
			desired: &corev1.ComponentFirmwareExpected{Result: &corev1.ComponentFirmwareExpected_Error{Error: &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_CONFIGURATION_UNAVAILABLE}}},
			want:    [2]rackFirmwareCoverage{{total: 1, unsupported: 1}, {total: 1, unavailable: 1}},
		},
		{
			// A defensive missing-result check differs from explicit unsupported
			// capability; normal callers apply readCoreFirmwareDetails first.
			name:    "missing result and unsupported expectations",
			actual:  &corev1.ComponentFirmwareInstalled{},
			desired: &corev1.ComponentFirmwareExpected{Result: &corev1.ComponentFirmwareExpected_Error{Error: &corev1.ComponentFirmwareError{Code: corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_UNSUPPORTED}}},
			want:    [2]rackFirmwareCoverage{{total: 1, unavailable: 1}, {total: 1, unsupported: 1}},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			member := rackFirmwareComponentDetails{componentType: devicetypes.ComponentTypeCompute, firmwareCoreDetails: firmwareCoreDetails{
				actual:  &pb.FirmwareActualDetails{Result: &pb.FirmwareActualDetails_Value{Value: test.actual}},
				desired: &pb.FirmwareDesiredDetails{Result: &pb.FirmwareDesiredDetails_Value{Value: test.desired}},
			}}

			actual, desired := summarizeRackFirmware([]rackFirmwareComponentDetails{member})

			require.Equal(t, test.want, [2]rackFirmwareCoverage{actual, desired})
		})
	}
}
