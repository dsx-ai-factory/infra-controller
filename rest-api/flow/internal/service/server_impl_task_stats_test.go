// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

type taskStatsStore struct {
	taskstore.Store
	tasks   []*taskdef.Task
	rackIDs []uuid.UUID
	err     error
}

func (s *taskStatsStore) ListNonTerminalTasksForRacks(
	_ context.Context,
	rackIDs []uuid.UUID,
) ([]*taskdef.Task, error) {
	s.rackIDs = append([]uuid.UUID{}, rackIDs...)
	return s.tasks, s.err
}

func TestPopulateTaskStats(t *testing.T) {
	rackID := uuid.New()
	componentID := uuid.New()
	wantStoreErr := errors.New("task store unavailable")

	tests := []struct {
		name               string
		store              *taskStatsStore
		racks              []*pb.Rack
		components         []*pb.Component
		wantErr            error
		wantRackIDs        []uuid.UUID
		wantRackStats      []*pb.TaskStats
		wantComponentStats []*pb.TaskStats
	}{
		{
			name:          "store error",
			store:         &taskStatsStore{err: wantStoreErr},
			racks:         []*pb.Rack{{Info: &pb.DeviceInfo{Id: &pb.UUID{Id: rackID.String()}}}},
			wantErr:       wantStoreErr,
			wantRackIDs:   []uuid.UUID{rackID},
			wantRackStats: []*pb.TaskStats{{}},
		},
		{
			name:               "no store returns zero counts",
			racks:              []*pb.Rack{{}},
			components:         []*pb.Component{{}},
			wantRackStats:      []*pb.TaskStats{{}},
			wantComponentStats: []*pb.TaskStats{{}},
		},
		{
			name: "counts non-terminal tasks by target",
			store: &taskStatsStore{tasks: []*taskdef.Task{
				{RackID: rackID, Status: taskcommon.TaskStatusWaiting},
				{
					RackID: rackID,
					Status: taskcommon.TaskStatusPending,
					Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
						devicetypes.ComponentTypeCompute: {componentID, componentID},
					}},
				},
				{
					RackID: rackID,
					Status: taskcommon.TaskStatusRunning,
					Attributes: taskcommon.TaskAttributes{ComponentsByType: map[devicetypes.ComponentType][]uuid.UUID{
						devicetypes.ComponentTypeCompute: {componentID},
					}},
				},
				{RackID: rackID, Status: taskcommon.TaskStatusCompleted},
			}},
			racks: []*pb.Rack{{Info: &pb.DeviceInfo{Id: &pb.UUID{Id: rackID.String()}}}},
			components: []*pb.Component{{
				Info:   &pb.DeviceInfo{Id: &pb.UUID{Id: componentID.String()}},
				RackId: &pb.UUID{Id: rackID.String()},
			}},
			wantRackIDs: []uuid.UUID{rackID},
			wantRackStats: []*pb.TaskStats{{
				WaitingTaskCount: 1,
				PendingTaskCount: 1,
				RunningTaskCount: 1,
			}},
			wantComponentStats: []*pb.TaskStats{{
				PendingTaskCount: 1,
				RunningTaskCount: 1,
			}},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := &FlowServerImpl{taskStore: tt.store}
			err := server.populateTaskStats(
				context.Background(),
				tt.racks,
				tt.components,
			)
			if tt.wantErr != nil {
				require.ErrorIs(t, err, tt.wantErr)
			} else {
				require.NoError(t, err)
			}

			if tt.store != nil {
				assert.Equal(t, tt.wantRackIDs, tt.store.rackIDs)
			}
			require.Len(t, tt.racks, len(tt.wantRackStats))
			for i, rack := range tt.racks {
				assert.Equal(t, tt.wantRackStats[i], rack.GetTaskStats())
			}
			require.Len(t, tt.components, len(tt.wantComponentStats))
			for i, component := range tt.components {
				assert.Equal(t, tt.wantComponentStats[i], component.GetTaskStats())
			}
		})
	}
}

func TestIncrementTaskStats(t *testing.T) {
	tests := []struct {
		name   string
		status taskcommon.TaskStatus
		want   *pb.TaskStats
	}{
		{name: "unknown", status: taskcommon.TaskStatusUnknown, want: &pb.TaskStats{}},
		{name: "waiting", status: taskcommon.TaskStatusWaiting, want: &pb.TaskStats{WaitingTaskCount: 1}},
		{name: "pending", status: taskcommon.TaskStatusPending, want: &pb.TaskStats{PendingTaskCount: 1}},
		{name: "running", status: taskcommon.TaskStatusRunning, want: &pb.TaskStats{RunningTaskCount: 1}},
		{name: "completed", status: taskcommon.TaskStatusCompleted, want: &pb.TaskStats{}},
		{name: "failed", status: taskcommon.TaskStatusFailed, want: &pb.TaskStats{}},
		{name: "terminated", status: taskcommon.TaskStatusTerminated, want: &pb.TaskStats{}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := &pb.TaskStats{}
			incrementTaskStats(got, tt.status)
			assert.Equal(t, tt.want, got)
		})
	}
}
