// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// TestPowerTaskWaitingContext keeps actual conflicts separate from earlier
// waiting work, so a displayed reference does not promise an execution order.
func TestPowerTaskWaitingContext(t *testing.T) {
	target := uuid.New()
	created := time.Date(2026, 10, 15, 10, 0, 0, 0, time.UTC)
	selection := taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
		devicetypes.ComponentTypeCompute: {target},
	}}
	incoming := taskdef.PowerTaskDetails{ID: uuid.New(), Attributes: selection, CreatedAt: created}
	tests := []struct {
		name     string
		metadata []taskdef.TaskContextMetadata
		want     *pb.PowerTaskWaitingContext
	}{
		// Successful empty evidence has a present message, unlike an older reply.
		{name: "no nearby work", want: &pb.PowerTaskWaitingContext{}},
		{
			// The earlier disjoint waiter is context because promotion stops at
			// the first conflict, even when later work targets different hardware.
			name: "active conflict and earlier disjoint work are distinct",
			metadata: []taskdef.TaskContextMetadata{
				// Running firmware overlaps the selected compute tray.
				{ID: uuid.UUID{15: 1}, Type: taskcommon.TaskTypeFirmwareControl, Status: taskcommon.TaskStatusRunning, Attributes: selection},
				// Pending power on a different compute tray does not conflict.
				{ID: uuid.UUID{15: 2}, Type: taskcommon.TaskTypePowerControl, Status: taskcommon.TaskStatusPending,
					Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {uuid.New()}}}},
				// Earlier non-power work remains informative without target overlap.
				{ID: uuid.UUID{15: 3}, Type: taskcommon.TaskTypeFirmwareControl, Status: taskcommon.TaskStatusWaiting, CreatedAt: created.Add(-time.Second),
					Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {uuid.New()}}}},
				// Bring-up conflicts across the rack even before it starts or when
				// its targets do not overlap this power task.
				{ID: uuid.UUID{15: 6}, Type: taskcommon.TaskTypeBringUp, Status: taskcommon.TaskStatusPending,
					Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{devicetypes.ComponentTypeCompute: {uuid.New()}}}},
				// Tied and later submissions do not prove earlier work.
				{ID: uuid.UUID{15: 4}, Type: taskcommon.TaskTypeBringUp, Status: taskcommon.TaskStatusWaiting, CreatedAt: created},
				{ID: uuid.UUID{15: 5}, Type: taskcommon.TaskTypeBringUp, Status: taskcommon.TaskStatusWaiting, CreatedAt: created.Add(time.Second)},
				// A task can have advanced between reads; never reference itself.
				{ID: incoming.ID, Type: taskcommon.TaskTypePowerControl, Status: taskcommon.TaskStatusRunning, Attributes: selection},
			},
			want: &pb.PowerTaskWaitingContext{
				ActiveConflicts: []*pb.TaskBrief{
					{Id: &pb.UUID{Id: (uuid.UUID{15: 1}).String()}, Kind: pb.TaskKind_TASK_KIND_FIRMWARE_CONTROL, Status: pb.TaskStatus_TASK_STATUS_RUNNING},
					{Id: &pb.UUID{Id: (uuid.UUID{15: 6}).String()}, Kind: pb.TaskKind_TASK_KIND_BRING_UP, Status: pb.TaskStatus_TASK_STATUS_PENDING},
				},
				EarlierWaiting: []*pb.TaskBrief{{Id: &pb.UUID{Id: (uuid.UUID{15: 3}).String()}, Kind: pb.TaskKind_TASK_KIND_FIRMWARE_CONTROL, Status: pb.TaskStatus_TASK_STATUS_WAITING}},
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := powerTaskWaitingContext(incoming, tc.metadata)

			require.True(t, proto.Equal(tc.want, got), "want %v; got %v", tc.want, got)
		})
	}
}

// TestFlowServerImpl_AddPowerTaskWaitingContext proves one bounded read serves
// every waiting power task, and failures preserve those tasks with explicit errors.
func TestFlowServerImpl_AddPowerTaskWaitingContext(t *testing.T) {
	tests := []struct {
		name      string
		metadata  bool
		readError error
		status    taskcommon.TaskStatus
		wantError *pb.PowerDetailsReadError
	}{
		// Successful empty evidence is still distinguishable from unavailable context.
		{name: "computed empty", status: taskcommon.TaskStatusWaiting},
		// One rack read must remain associated with each waiter after projection.
		{name: "distinct contexts share one read", status: taskcommon.TaskStatusWaiting, metadata: true},
		// Non-waiting work has no context and does not spend a query on it.
		{name: "running is not applicable", status: taskcommon.TaskStatusRunning},
		// Dependency text must not become a task field.
		{name: "read failure", status: taskcommon.TaskStatusWaiting, readError: errors.New("private-data"), wantError: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED.Enum()},
		// Too much non-power work does not erase the primary power list.
		{name: "metadata overflow", status: taskcommon.TaskStatusWaiting, readError: &taskcommon.TaskContextLimitError{}, wantError: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum()},
		// The shared read deadline remains visible as a distinct failure.
		{name: "metadata deadline", status: taskcommon.TaskStatusWaiting, readError: context.DeadlineExceeded, wantError: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED.Enum()},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			rackID := uuid.New()
			created := time.Date(2026, 10, 15, 10, 0, 0, 0, time.UTC)
			tasks := []taskdef.PowerTaskDetails{
				{ID: uuid.UUID{15: 1}, RackID: rackID, Status: tc.status, CreatedAt: created},
				{ID: uuid.UUID{15: 2}, RackID: rackID, Status: tc.status, CreatedAt: created.Add(2 * time.Second)},
			}
			entries := []*pb.PowerTaskDetails{{Id: &pb.UUID{Id: tasks[0].ID.String()}}, {Id: &pb.UUID{Id: tasks[1].ID.String()}}}
			store := &powerDetailsTaskStore{contextRead: func(context.Context) ([]taskdef.TaskContextMetadata, error) {
				if tc.metadata {
					// The intervening non-power task is earlier only for the second waiter.
					return []taskdef.TaskContextMetadata{{ID: uuid.UUID{15: 3}, RackID: rackID,
						Type: taskcommon.TaskTypeFirmwareControl, Status: taskcommon.TaskStatusWaiting, CreatedAt: created.Add(time.Second)}}, nil
				}
				return nil, tc.readError
			}}
			server := &FlowServerImpl{taskStore: store}

			server.addPowerTaskWaitingContext(t.Context(), rackID, tasks, entries)

			if tc.status != taskcommon.TaskStatusWaiting {
				require.Zero(t, store.contextCalls)
				require.Nil(t, entries[0].WaitingContext)
				require.Nil(t, entries[1].WaitingContext)
				return
			}
			require.Equal(t, 1, store.contextCalls)
			require.Equal(t, rackID, store.contextRackID)
			for index, entry := range entries {
				require.Equal(t, tasks[index].ID.String(), entry.GetId().GetId())
				require.NotNil(t, entry.WaitingContext)
				require.Equal(t, tc.wantError, entry.WaitingContext.Error)
				require.Empty(t, entry.WaitingContext.ActiveConflicts)
				if tc.metadata && index == 1 {
					want := &pb.TaskBrief{Id: &pb.UUID{Id: (uuid.UUID{15: 3}).String()}, Kind: pb.TaskKind_TASK_KIND_FIRMWARE_CONTROL, Status: pb.TaskStatus_TASK_STATUS_WAITING}
					require.Len(t, entry.WaitingContext.EarlierWaiting, 1)
					require.True(t, proto.Equal(want, entry.WaitingContext.EarlierWaiting[0]))
				} else {
					require.Empty(t, entry.WaitingContext.EarlierWaiting)
				}
			}
		})
	}
}

// TestPowerTaskDetails_WaitingContext checks presence through protobuf encoding,
// where older replies must remain distinguishable from computed empty context.
func TestPowerTaskDetails_WaitingContext(t *testing.T) {
	for _, tc := range []struct {
		name    string
		context *pb.PowerTaskWaitingContext
	}{
		// An older Waiting response contains no context field on the wire.
		{name: "older response"},
		// A present empty message explicitly means the check succeeded.
		{name: "computed empty", context: &pb.PowerTaskWaitingContext{}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			encoded, err := proto.Marshal(&pb.PowerTaskDetails{Status: pb.TaskStatus_TASK_STATUS_WAITING, WaitingContext: tc.context})
			require.NoError(t, err)
			var decoded pb.PowerTaskDetails
			require.NoError(t, proto.Unmarshal(encoded, &decoded))

			require.Equal(t, tc.context == nil, decoded.WaitingContext == nil)
		})
	}
}
